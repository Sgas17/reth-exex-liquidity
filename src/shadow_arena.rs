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

use crate::types::{PoolIdentifier, PoolUpdateMessage, Protocol, ReorgEpilogueUpdate};
use arena_layout::{
    AnyEkuboPool, AnyUniswapV3Pool, AnyUniswapV4Pool, CurveStablePoolData, CurveTricryptoPoolData,
    CurveTwoCryptoPoolData, PoolTier, SIGNAL_REASON_LIVE_BLOCK_APPLY,
    SIGNAL_REASON_LIVE_BLOCK_EMPTY,
};
use arena_writer::{ArenaMmap, SharedArenaWriter, WriterError};
use std::collections::HashSet;
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
    /// Pools whose tick array overflowed their tier this block and must be
    /// re-tiered (promoted). Drained at the block boundary by the ExEx, which
    /// re-scrapes them and calls `retier_*`.
    retier_pending: HashSet<(Protocol, PoolIdentifier)>,
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
            retier_pending: HashSet::new(),
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
        self.apply_event_unguarded(event)
    }

    /// Apply a reorg revert/replay pool update (ITE-16 step 3d), **bypassing** the
    /// replay guard. A reorg can cross the startup hydration anchor; its
    /// revert/replay events must still adjust state baked into the hydration
    /// snapshot. This is correct because relative-delta protocols (V2/Balancer/
    /// tick liquidity) invert exactly regardless of baseline, and absolute-state
    /// protocols (V3/V4/Ekubo slot0, Fluid) are restored by the slot0/fluid-final
    /// epilogue. For reorgs that do not cross the anchor the bypass is a no-op
    /// (all blocks are already `> scraped_at_block`).
    pub fn apply_reorg_event(
        &mut self,
        event: &PoolUpdateMessage,
    ) -> std::result::Result<bool, crate::shadow_apply::ApplyError> {
        self.apply_event_unguarded(event)
    }

    fn apply_event_unguarded(
        &mut self,
        event: &PoolUpdateMessage,
    ) -> std::result::Result<bool, crate::shadow_apply::ApplyError> {
        let mut overflowed = false;
        let applied = {
            let mut writer = SharedArenaWriter::new(self.arena.region_mut());
            crate::shadow_apply::apply_live_event(&mut writer, event, &mut overflowed)?
        };
        if applied {
            self.applied_this_block += 1;
        }
        if overflowed {
            // Queue for promotion at the block boundary (re-scrape + re-tier).
            self.retier_pending
                .insert((event.protocol, event.pool_id.clone()));
        }
        Ok(applied)
    }

    /// Drain the set of pools that overflowed their tier and need promotion. The
    /// ExEx re-scrapes each at the current block and calls the matching `retier_*`.
    pub fn take_retier_pending(&mut self) -> Vec<(Protocol, PoolIdentifier)> {
        self.retier_pending.drain().collect()
    }

    /// Promote a V3 pool to a roomier tier (ITE-16 Phase 2). The fresh `pool` was
    /// rebuilt from a full re-scrape, so `determine_tier` already placed it in the
    /// right tier. This is failure-safe: the old slot is removed only after the
    /// target tier is confirmed to accept the pool, so a full target tier leaves
    /// the (overflowed) lower-tier pool in place rather than losing it — the next
    /// overflow re-queues the promotion. The preflight is same-tier-aware: when the
    /// pool already occupies a slot in the (saturated) target tier — a transient
    /// overflow that re-scrapes back to the same tier — the rewrite is allowed
    /// because removing the old assignment frees the exact slot the re-add reuses.
    /// On success it is an in-place topology change (remove old + add new + bump
    /// `slot_version`; no double buffer).
    pub fn retier_v3(
        &mut self,
        addr: [u8; 20],
        pool: AnyUniswapV3Pool,
    ) -> std::result::Result<(), crate::shadow_apply::ApplyError> {
        let tier = match &pool {
            AnyUniswapV3Pool::Low(_) => PoolTier::Low,
            AnyUniswapV3Pool::Active(_) => PoolTier::Active,
            AnyUniswapV3Pool::Popular(_) => PoolTier::Popular,
            AnyUniswapV3Pool::Major(_) => PoolTier::Major,
        };
        let mut writer = SharedArenaWriter::new(self.arena.region_mut());
        // Same-tier-aware preflight: a saturated target tier is acceptable when
        // the pool ALREADY occupies a slot in it (a transient overflow that
        // re-scrapes back to the same tier). Removing the old assignment frees
        // exactly the slot the re-add reuses, so requiring a *separate* free slot
        // would wrongly reject a valid in-place rewrite and strand the pool with
        // its overflowed snapshot.
        let current_tier = writer.get_v3_pool(&addr).map(|p| match p {
            AnyUniswapV3Pool::Low(_) => PoolTier::Low,
            AnyUniswapV3Pool::Active(_) => PoolTier::Active,
            AnyUniswapV3Pool::Popular(_) => PoolTier::Popular,
            AnyUniswapV3Pool::Major(_) => PoolTier::Major,
        });
        if !writer.v3_tier_has_free_slot(tier) && current_tier != Some(tier) {
            return Err(crate::shadow_apply::ApplyError::Writer(
                WriterError::NoFreeSlots(tier),
            ));
        }
        writer
            .remove_pool(addr)
            .map_err(crate::shadow_apply::ApplyError::Writer)?;
        writer
            .add_v3_pool(pool)
            .map_err(crate::shadow_apply::ApplyError::Writer)?;
        writer.signal_topology_change();
        Ok(())
    }

    /// Promote a V4 pool to a roomier tier — failure-safe (see [`Self::retier_v3`]).
    pub fn retier_v4(
        &mut self,
        pool_id: [u8; 32],
        pool: AnyUniswapV4Pool,
    ) -> std::result::Result<(), crate::shadow_apply::ApplyError> {
        let tier = match &pool {
            AnyUniswapV4Pool::Low(_) => PoolTier::Low,
            AnyUniswapV4Pool::Active(_) => PoolTier::Active,
            AnyUniswapV4Pool::Popular(_) => PoolTier::Popular,
            AnyUniswapV4Pool::Major(_) => PoolTier::Major,
        };
        let mut writer = SharedArenaWriter::new(self.arena.region_mut());
        // Same-tier-aware preflight (see [`Self::retier_v3`]).
        let current_tier = writer.get_v4_pool(&pool_id).map(|p| match p {
            AnyUniswapV4Pool::Low(_) => PoolTier::Low,
            AnyUniswapV4Pool::Active(_) => PoolTier::Active,
            AnyUniswapV4Pool::Popular(_) => PoolTier::Popular,
            AnyUniswapV4Pool::Major(_) => PoolTier::Major,
        });
        if !writer.v4_tier_has_free_slot(tier) && current_tier != Some(tier) {
            return Err(crate::shadow_apply::ApplyError::Writer(
                WriterError::NoFreeSlots(tier),
            ));
        }
        writer
            .remove_pool_v4(pool_id)
            .map_err(crate::shadow_apply::ApplyError::Writer)?;
        writer
            .add_v4_pool(pool)
            .map_err(crate::shadow_apply::ApplyError::Writer)?;
        writer.signal_topology_change();
        Ok(())
    }

    /// Promote an Ekubo pool to a roomier tier — failure-safe (see [`Self::retier_v3`]).
    pub fn retier_ekubo(
        &mut self,
        pool_id: [u8; 32],
        pool: AnyEkuboPool,
    ) -> std::result::Result<(), crate::shadow_apply::ApplyError> {
        let tier = match &pool {
            AnyEkuboPool::Low(_) => PoolTier::Low,
            AnyEkuboPool::Active(_) => PoolTier::Active,
            AnyEkuboPool::Popular(_) => PoolTier::Popular,
            AnyEkuboPool::Major(_) => PoolTier::Major,
        };
        let mut writer = SharedArenaWriter::new(self.arena.region_mut());
        // Same-tier-aware preflight (see [`Self::retier_v3`]).
        let current_tier = writer.get_ekubo_pool(&pool_id).map(|p| match p {
            AnyEkuboPool::Low(_) => PoolTier::Low,
            AnyEkuboPool::Active(_) => PoolTier::Active,
            AnyEkuboPool::Popular(_) => PoolTier::Popular,
            AnyEkuboPool::Major(_) => PoolTier::Major,
        });
        if !writer.ekubo_tier_has_free_slot(tier) && current_tier != Some(tier) {
            return Err(crate::shadow_apply::ApplyError::Writer(
                WriterError::NoFreeSlots(tier),
            ));
        }
        writer
            .remove_pool_v4(pool_id)
            .map_err(crate::shadow_apply::ApplyError::Writer)?;
        writer
            .add_ekubo_pool(pool)
            .map_err(crate::shadow_apply::ApplyError::Writer)?;
        writer.signal_topology_change();
        Ok(())
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
        AnyEkuboPool, AnyUniswapV3Pool, AnyUniswapV4Pool, SharedArenaRegion,
        UniswapV3ActivePoolData, UniswapV3LowPoolData, UniswapV3MajorPoolData,
        UniswapV3PopularPoolData, UniswapV4LowPoolData, UniswapV4MajorPoolData,
        SHARED_ARENA_VERSION, V3_MAJOR_CAPACITY, V4_MAJOR_CAPACITY,
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

    fn ekubo_liquidity_event(
        pool_id: [u8; 32],
        block: u64,
        delta: i128,
        sqrt_ratio: u64,
        tick: i32,
        liquidity: u128,
        is_revert: bool,
    ) -> PoolUpdateMessage {
        PoolUpdateMessage {
            pool_id: PoolIdentifier::PoolId(pool_id),
            protocol: Protocol::Ekubo,
            update_type: if delta >= 0 {
                UpdateType::Mint
            } else {
                UpdateType::Burn
            },
            block_number: block,
            block_timestamp: 0,
            tx_index: 0,
            log_index: 0,
            is_revert,
            update: PoolUpdate::EkuboLiquidity {
                tick_lower: -10,
                tick_upper: 10,
                liquidity_delta: delta,
                sqrt_ratio: U256::from(sqrt_ratio),
                liquidity,
                tick,
            },
        }
    }

    /// 3d (round-10 fix): reverting an Ekubo `PositionUpdated` inverts the tick
    /// delta but must NOT write the reverted fork's `stateAfter` into slot0; the
    /// reorg slot0-final epilogue restores the canonical slot0 instead.
    #[test]
    fn ekubo_position_revert_keeps_slot0_until_epilogue() {
        let path = temp_arena_path("ekubo_revert");
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

        // Forward position update: ticks gain +5000, slot0 → stateAfter (9999).
        assert!(shadow
            .apply_reorg_event(&ekubo_liquidity_event(
                ekubo_id, 101, 5_000, 9_999, 33, 350_000, false,
            ))
            .expect("forward"));

        // Revert (same position): tick delta inverted, but the revert's stateAfter
        // (1234 here) must NOT be written — slot0 stays at the forward value.
        assert!(shadow
            .apply_reorg_event(&ekubo_liquidity_event_revert(
                ekubo_id, 102, 5_000, 1_234, 7, 1,
            ))
            .expect("revert"));

        {
            let writer = SharedArenaWriter::new(shadow.arena.region_mut());
            let got = writer.get_ekubo_pool(&ekubo_id).expect("ekubo pool");
            assert_eq!(
                got.sqrt_price_x96(),
                U256::from(9_999u64),
                "revert must not write reverted stateAfter"
            );
            let AnyEkuboPool::Low(p) = got else {
                panic!("expected Low Ekubo");
            };
            assert_eq!(p.tick_count, 0, "tick delta inverted back to empty");
        }

        // Epilogue restores the canonical post-reorg slot0 (5555).
        let epilogue = ReorgEpilogueUpdate::Slot0Final {
            pool_id: PoolIdentifier::PoolId(ekubo_id),
            protocol: Protocol::Ekubo,
            state: Slot0State {
                sqrt_price_x96: U256::from(5_555u64),
                liquidity: 222_000,
                tick: 12,
            },
        };
        assert!(shadow.apply_reorg_epilogue(&epilogue).expect("epilogue"));

        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        let got = writer.get_ekubo_pool(&ekubo_id).expect("ekubo pool");
        assert_eq!(got.sqrt_price_x96(), U256::from(5_555u64));
        assert_eq!(got.tick(), 12);
        assert_eq!(got.liquidity(), 222_000);

        let _ = std::fs::remove_file(&path);
    }

    /// 3d (round-10 fix): reorg revert/replay events bypass the startup replay
    /// guard, so an anchor-crossing reorg still adjusts hydrated state — whereas
    /// the normal live path skips events at/below the anchor.
    #[test]
    fn reorg_event_bypasses_replay_guard() {
        let path = temp_arena_path("anchor_cross");
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

        // Live path at the anchor block is guarded → skipped, reserves unchanged.
        assert!(!shadow
            .apply_live_event(&v2_swap_event(addr(0xC2), 100, 500, -300))
            .expect("guarded"));
        {
            let writer = SharedArenaWriter::new(shadow.arena.region_mut());
            let pool = writer.get_v2_pool(&addr(0xC2)).expect("v2 pool");
            assert_eq!(pool.reserve0, 1_000);
        }

        // Reorg path at the same anchor block bypasses the guard → applies.
        assert!(shadow
            .apply_reorg_event(&v2_swap_event(addr(0xC2), 100, 500, -300))
            .expect("bypass"));
        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        let pool = writer.get_v2_pool(&addr(0xC2)).expect("v2 pool");
        assert_eq!(pool.reserve0, 1_500);
        assert_eq!(pool.reserve1, 1_700);

        let _ = std::fs::remove_file(&path);
    }

    fn ekubo_liquidity_event_revert(
        pool_id: [u8; 32],
        block: u64,
        delta: i128,
        sqrt_ratio: u64,
        tick: i32,
        liquidity: u128,
    ) -> PoolUpdateMessage {
        ekubo_liquidity_event(pool_id, block, delta, sqrt_ratio, tick, liquidity, true)
    }

    fn v3_low_pool(address: [u8; 20]) -> UniswapV3LowPoolData {
        let mut v3 = UniswapV3LowPoolData::default();
        v3.common.pool_id = address;
        v3.common.is_active.store(true, Ordering::Release);
        v3.sqrt_price_x96 = U256::from(1_000u64);
        v3.tick = 0;
        v3.liquidity = 100_000;
        v3.fee = 500;
        v3.tick_spacing = 10;
        v3.token0_decimals = 6;
        v3.token1_decimals = 18;
        v3
    }

    fn v3_liquidity_event(
        address: [u8; 20],
        block: u64,
        delta: i128,
        is_revert: bool,
    ) -> PoolUpdateMessage {
        PoolUpdateMessage {
            pool_id: PoolIdentifier::Address(Address::from(address)),
            protocol: Protocol::UniswapV3,
            update_type: if delta >= 0 {
                UpdateType::Mint
            } else {
                UpdateType::Burn
            },
            block_number: block,
            block_timestamp: 0,
            tx_index: 0,
            log_index: 0,
            is_revert,
            update: PoolUpdate::V3Liquidity {
                tick_lower: -10,
                tick_upper: 10,
                liquidity_delta: delta,
            },
        }
    }

    fn v3_tick_count(shadow: &mut ShadowArena, address: &[u8; 20]) -> u16 {
        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        match writer.get_v3_pool(address).expect("v3 pool") {
            AnyUniswapV3Pool::Low(p) => p.tick_count,
            _ => panic!("expected Low-tier V3 pool"),
        }
    }

    fn v3_tick_gross(shadow: &mut ShadowArena, address: &[u8; 20], tick: i32) -> Option<u128> {
        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        match writer.get_v3_pool(address).expect("v3 pool") {
            AnyUniswapV3Pool::Low(p) => {
                let n = p.tick_count as usize;
                p.ticks[..n]
                    .iter()
                    .find(|(t, _, _)| *t == tick)
                    .map(|(_, gross, _)| *gross)
            }
            _ => panic!("expected Low-tier V3 pool"),
        }
    }

    fn v3_shadow_after_old_fork_mint_burn(
        tag: &str,
        address: [u8; 20],
        mint: &PoolUpdateMessage,
        burn: &PoolUpdateMessage,
    ) -> (ShadowArena, PathBuf) {
        let path = temp_arena_path(tag);
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");
        shadow.hydrate_startup(
            100,
            &[],
            &[UniswapV3Hydration {
                address,
                pool: AnyUniswapV3Pool::Low(v3_low_pool(address)),
            }],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        // Old fork: mint then burn the same range → ticks end empty (gross 0).
        shadow.apply_reorg_event(mint).expect("old-fork mint");
        shadow.apply_reorg_event(burn).expect("old-fork burn");
        (shadow, path)
    }

    /// 3d (round-11 fix): reorg old-block reverts must un-apply in REVERSE
    /// execution order. An old fork that mints then burns the same tick range ends
    /// with gross 0 (tick removed). Reverting the burn FIRST re-adds the tick with
    /// a plausible gross; reverting the mint first (the old forward order) re-adds
    /// the absent tick with a NEGATIVE delta that wraps through `as u128` to a huge
    /// gross — observable at the per-block reorg signal between the two reverts.
    /// The final state self-heals once both reverts land, so the corruption is the
    /// transient mid-reorg value, which the reverse order avoids.
    #[test]
    fn reorg_revert_reverse_order_keeps_v3_ticks_clean() {
        let a = addr(0x37);
        let mint = v3_liquidity_event(a, 101, 5_000, false);
        let burn = v3_liquidity_event(a, 102, -5_000, false);
        let mut mint_rev = mint.clone();
        mint_rev.is_revert = true;
        let mut burn_rev = burn.clone();
        burn_rev.is_revert = true;

        // FIXED order (what the reversed reorg loop now emits): revert burn first.
        {
            let (mut shadow, path) =
                v3_shadow_after_old_fork_mint_burn("v3order_rev", a, &mint, &burn);
            assert_eq!(v3_tick_count(&mut shadow, &a), 0, "old fork ends clean");

            shadow.apply_reorg_event(&burn_rev).expect("revert burn");
            // Intermediate state (what a reader sees at this block's signal) is
            // plausible: the re-added tick carries the burned liquidity, not a wrap.
            assert_eq!(
                v3_tick_gross(&mut shadow, &a, -10),
                Some(5_000),
                "reverse order: plausible intermediate gross"
            );
            shadow.apply_reorg_event(&mint_rev).expect("revert mint");
            assert_eq!(v3_tick_count(&mut shadow, &a), 0, "ends clean");
            let _ = std::fs::remove_file(&path);
        }

        // BUGGY forward order (for contrast): reverting the mint first re-inserts
        // the absent tick with a negative delta, wrapping gross to a huge value.
        {
            let (mut shadow, path) =
                v3_shadow_after_old_fork_mint_burn("v3order_fwd", a, &mint, &burn);
            shadow
                .apply_reorg_event(&mint_rev)
                .expect("revert mint (forward)");
            let gross = v3_tick_gross(&mut shadow, &a, -10).expect("tick re-inserted");
            assert!(
                gross > u128::from(u64::MAX),
                "forward order wraps gross to a huge value (the bug the reversal fixes), got {gross}"
            );
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Phase 2: a V3 pool whose tick array overflows its tier is queued for
    /// promotion (the writer reports overflow → `retier_pending`).
    #[test]
    fn overflow_queues_pool_for_retier() {
        let path = temp_arena_path("overflow_queue");
        let a = addr(0x66);
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");
        shadow.hydrate_startup(
            100,
            &[],
            &[UniswapV3Hydration {
                address: a,
                pool: AnyUniswapV3Pool::Low(v3_low_pool(a)),
            }],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
        );

        // Low holds 50 ticks; 26 distinct mints (2 new ticks each) overflow it.
        for i in 0..26i32 {
            let ev = PoolUpdateMessage {
                pool_id: PoolIdentifier::Address(Address::from(a)),
                protocol: Protocol::UniswapV3,
                update_type: UpdateType::Mint,
                block_number: 101,
                block_timestamp: 0,
                tx_index: 0,
                log_index: 0,
                is_revert: false,
                update: PoolUpdate::V3Liquidity {
                    tick_lower: i * 100,
                    tick_upper: i * 100 + 50,
                    liquidity_delta: 1_000,
                },
            };
            shadow.apply_live_event(&ev).expect("apply mint");
        }

        let pending = shadow.take_retier_pending();
        assert_eq!(pending.len(), 1, "overflowed pool queued for promotion");
        assert_eq!(pending[0].0, Protocol::UniswapV3);
        // Draining clears it.
        assert!(shadow.take_retier_pending().is_empty());

        let _ = std::fs::remove_file(&path);
    }

    /// Phase 2: `retier_v3` promotes a pool to a bigger tier in place — the old
    /// slot is freed and the pool reads back from the new tier.
    #[test]
    fn retier_promotes_v3_to_a_bigger_tier() {
        let path = temp_arena_path("retier_v3");
        let a = addr(0x55);
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");
        shadow.hydrate_startup(
            100,
            &[],
            &[UniswapV3Hydration {
                address: a,
                pool: AnyUniswapV3Pool::Low(v3_low_pool(a)),
            }],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        {
            let writer = SharedArenaWriter::new(shadow.arena.region_mut());
            assert!(matches!(
                writer.get_v3_pool(&a),
                Some(AnyUniswapV3Pool::Low(_))
            ));
        }

        // Re-scrape would yield a bigger-tier pool; here build an Active one.
        let mut active = UniswapV3ActivePoolData::default();
        active.common.pool_id = a;
        active.common.is_active.store(true, Ordering::Release);
        active.sqrt_price_x96 = U256::from(4_242u64);
        active.tick = 7;
        active.tick_spacing = 10;
        active.token0_decimals = 6;
        active.token1_decimals = 18;
        shadow
            .retier_v3(a, AnyUniswapV3Pool::Active(active))
            .expect("retier");

        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        let got = writer.get_v3_pool(&a).expect("pool present after retier");
        assert!(
            matches!(got, AnyUniswapV3Pool::Active(_)),
            "pool promoted to the Active tier"
        );
        assert_eq!(got.sqrt_price_x96(), U256::from(4_242u64));

        let _ = std::fs::remove_file(&path);
    }

    /// Phase 2 failure-safety (round-16 Critical 2): when the target tier has no
    /// free slot, `retier_v3` must fail WITHOUT removing the existing assignment.
    /// Losing a hot pool is worse than keeping its overflowed lower-tier snapshot,
    /// so the original pool stays readable and a later overflow re-queues it.
    #[test]
    fn retier_into_full_tier_keeps_existing_pool() {
        let path = temp_arena_path("retier_full");
        let a = [0xCC_u8; 20];
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");

        // Hydrate the pool we will try (and fail) to promote into the Major tier.
        let mut popular = UniswapV3PopularPoolData::default();
        popular.common.pool_id = a;
        popular.common.is_active.store(true, Ordering::Release);
        popular.sqrt_price_x96 = U256::from(9_001u64);
        popular.tick_spacing = 10;
        popular.token0_decimals = 6;
        popular.token1_decimals = 18;
        shadow.hydrate_startup(
            100,
            &[],
            &[UniswapV3Hydration {
                address: a,
                pool: AnyUniswapV3Pool::Popular(popular),
            }],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
        );

        // Saturate every Major slot so a promotion into Major cannot fit.
        {
            let mut writer = SharedArenaWriter::new(shadow.arena.region_mut());
            for i in 0..V3_MAJOR_CAPACITY as u16 {
                let mut id = [0u8; 20];
                id[18] = (i >> 8) as u8;
                id[19] = i as u8;
                let mut major = UniswapV3MajorPoolData::default();
                major.common.pool_id = id;
                major.common.is_active.store(true, Ordering::Release);
                major.tick_spacing = 10;
                major.token0_decimals = 6;
                major.token1_decimals = 18;
                writer
                    .add_v3_pool(AnyUniswapV3Pool::Major(major))
                    .expect("fill major slot");
            }
            assert!(
                !writer.v3_tier_has_free_slot(PoolTier::Major),
                "Major tier saturated"
            );
        }

        // A re-scrape that lands the pool in the (now full) Major tier must fail
        // the promotion rather than evict the live Popular assignment.
        let mut major = UniswapV3MajorPoolData::default();
        major.common.pool_id = a;
        major.common.is_active.store(true, Ordering::Release);
        major.sqrt_price_x96 = U256::from(424_242u64);
        major.tick_spacing = 10;
        major.token0_decimals = 6;
        major.token1_decimals = 18;
        let res = shadow.retier_v3(a, AnyUniswapV3Pool::Major(major));
        assert!(res.is_err(), "promotion into a full tier is rejected");

        // The original Popular pool is untouched and still readable.
        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        let got = writer.get_v3_pool(&a).expect("pool kept on failed retier");
        assert!(
            matches!(got, AnyUniswapV3Pool::Popular(_)),
            "failed promotion leaves the pool in its original tier"
        );
        assert_eq!(got.sqrt_price_x96(), U256::from(9_001u64));

        let _ = std::fs::remove_file(&path);
    }

    /// Phase 2 (round-16 Critical 1): overflow delivered via the REORG apply path
    /// (`apply_reorg_event`, which bypasses the replay guard) must queue the pool
    /// for promotion exactly like the committed-block path, so reorg blocks do not
    /// leave the shadow arena silently truncated until an unrelated later block.
    #[test]
    fn reorg_overflow_queues_pool_for_retier() {
        let path = temp_arena_path("reorg_overflow_queue");
        let a = addr(0x77);
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");
        shadow.hydrate_startup(
            100,
            &[],
            &[UniswapV3Hydration {
                address: a,
                pool: AnyUniswapV3Pool::Low(v3_low_pool(a)),
            }],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
        );

        // 26 distinct mints (2 new ticks each) overflow the 50-tick Low tier. They
        // arrive at block 50 — below the hydration anchor — to prove the reorg path
        // bypasses the replay guard yet still records the overflow for promotion.
        for i in 0..26i32 {
            let ev = PoolUpdateMessage {
                pool_id: PoolIdentifier::Address(Address::from(a)),
                protocol: Protocol::UniswapV3,
                update_type: UpdateType::Mint,
                block_number: 50,
                block_timestamp: 0,
                tx_index: 0,
                log_index: 0,
                is_revert: false,
                update: PoolUpdate::V3Liquidity {
                    tick_lower: i * 100,
                    tick_upper: i * 100 + 50,
                    liquidity_delta: 1_000,
                },
            };
            shadow.apply_reorg_event(&ev).expect("apply reorg mint");
        }

        let pending = shadow.take_retier_pending();
        assert_eq!(
            pending.len(),
            1,
            "reorg-delivered overflow queued for promotion"
        );
        assert_eq!(pending[0].0, Protocol::UniswapV3);

        let _ = std::fs::remove_file(&path);
    }

    /// Phase 2 same-tier failure-safety (round-17 Critical 2): when the target tier
    /// is saturated ONLY because the pool's own slot occupies it (a transient
    /// overflow that re-scrapes back to the same tier), the rewrite must succeed —
    /// removing the old assignment frees the exact slot the re-add reuses. Without
    /// the same-tier-aware preflight this would be rejected, stranding the pool with
    /// its overflowed snapshot.
    #[test]
    fn retier_same_tier_rewrites_when_tier_saturated_v3() {
        let path = temp_arena_path("retier_same_tier_v3");
        let a = [0xD1_u8; 20];
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");

        // Fill every Major slot; the last one is our pool `a`.
        {
            let mut writer = SharedArenaWriter::new(shadow.arena.region_mut());
            for i in 0..V3_MAJOR_CAPACITY as u16 {
                let id = if i + 1 == V3_MAJOR_CAPACITY as u16 {
                    a
                } else {
                    let mut x = [0u8; 20];
                    x[18] = (i >> 8) as u8;
                    x[19] = i as u8;
                    x
                };
                let mut major = UniswapV3MajorPoolData::default();
                major.common.pool_id = id;
                major.common.is_active.store(true, Ordering::Release);
                major.sqrt_price_x96 = U256::from(1u64);
                major.tick_spacing = 10;
                major.token0_decimals = 6;
                major.token1_decimals = 18;
                writer
                    .add_v3_pool(AnyUniswapV3Pool::Major(major))
                    .expect("fill major slot");
            }
            assert!(
                !writer.v3_tier_has_free_slot(PoolTier::Major),
                "Major tier saturated"
            );
        }

        // Re-scrape lands `a` back in Major (a valid Major-sized snapshot). The tier
        // is full, but `a` already owns a Major slot, so the rewrite succeeds.
        let mut major = UniswapV3MajorPoolData::default();
        major.common.pool_id = a;
        major.common.is_active.store(true, Ordering::Release);
        major.sqrt_price_x96 = U256::from(123_456u64);
        major.tick_spacing = 10;
        major.token0_decimals = 6;
        major.token1_decimals = 18;
        shadow
            .retier_v3(a, AnyUniswapV3Pool::Major(major))
            .expect("same-tier rewrite into a saturated tier");

        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        let got = writer
            .get_v3_pool(&a)
            .expect("pool present after same-tier retier");
        assert!(matches!(got, AnyUniswapV3Pool::Major(_)));
        assert_eq!(
            got.sqrt_price_x96(),
            U256::from(123_456u64),
            "rewritten with the fresh snapshot"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Same-tier saturated rewrite for a pool-id-keyed protocol (V4) — round-17
    /// Critical 2 coverage across the V4 path.
    #[test]
    fn retier_same_tier_rewrites_when_tier_saturated_v4() {
        let path = temp_arena_path("retier_same_tier_v4");
        let pid = [0xD2_u8; 32];
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");

        {
            let mut writer = SharedArenaWriter::new(shadow.arena.region_mut());
            for i in 0..V4_MAJOR_CAPACITY as u16 {
                let id = if i + 1 == V4_MAJOR_CAPACITY as u16 {
                    pid
                } else {
                    let mut x = [0u8; 32];
                    x[30] = (i >> 8) as u8;
                    x[31] = i as u8;
                    x
                };
                let mut major = UniswapV4MajorPoolData::default();
                major.pool_id = id;
                major.common.is_active.store(true, Ordering::Release);
                major.sqrt_price_x96 = U256::from(1u64);
                major.tick_spacing = 10;
                major.token0_decimals = 6;
                major.token1_decimals = 18;
                writer
                    .add_v4_pool(AnyUniswapV4Pool::Major(major))
                    .expect("fill v4 major slot");
            }
            assert!(
                !writer.v4_tier_has_free_slot(PoolTier::Major),
                "V4 Major tier saturated"
            );
        }

        let mut major = UniswapV4MajorPoolData::default();
        major.pool_id = pid;
        major.common.is_active.store(true, Ordering::Release);
        major.sqrt_price_x96 = U256::from(987_654u64);
        major.tick_spacing = 10;
        major.token0_decimals = 6;
        major.token1_decimals = 18;
        shadow
            .retier_v4(pid, AnyUniswapV4Pool::Major(major))
            .expect("same-tier v4 rewrite into a saturated tier");

        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        match writer
            .get_v4_pool(&pid)
            .expect("v4 pool present after retier")
        {
            AnyUniswapV4Pool::Major(p) => {
                assert_eq!(p.sqrt_price_x96, U256::from(987_654u64));
            }
            _ => panic!("expected Major-tier V4 pool"),
        }

        let _ = std::fs::remove_file(&path);
    }

    /// Phase 2 (round-17 Critical 1): overflow promotions are accumulated across the
    /// WHOLE reorg sequence and drained once at the end, so a pool overflowing on an
    /// early block and a different pool overflowing on a later block both survive to
    /// the single end-of-reorg drain — and a pool touched on multiple blocks is
    /// queued only once. The per-block drain removed this round re-scraped from a
    /// mid-sequence snapshot that later deltas then double-applied.
    #[test]
    fn reorg_overflow_pending_accumulates_across_blocks() {
        let path = temp_arena_path("reorg_accum");
        let a = addr(0x71);
        let b = addr(0x72);
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");
        shadow.hydrate_startup(
            100,
            &[],
            &[
                UniswapV3Hydration {
                    address: a,
                    pool: AnyUniswapV3Pool::Low(v3_low_pool(a)),
                },
                UniswapV3Hydration {
                    address: b,
                    pool: AnyUniswapV3Pool::Low(v3_low_pool(b)),
                },
            ],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
        );

        let mk = |pool: [u8; 20], block: u64, i: i32| PoolUpdateMessage {
            pool_id: PoolIdentifier::Address(Address::from(pool)),
            protocol: Protocol::UniswapV3,
            update_type: UpdateType::Mint,
            block_number: block,
            block_timestamp: 0,
            tx_index: 0,
            log_index: 0,
            is_revert: false,
            update: PoolUpdate::V3Liquidity {
                tick_lower: i * 100,
                tick_upper: i * 100 + 50,
                liquidity_delta: 1_000,
            },
        };

        // Block 50: pool A overflows. Block 51: pool B overflows. Block 52: pool A
        // is touched again. No per-block drain runs — the queue must survive.
        for i in 0..26i32 {
            shadow.apply_reorg_event(&mk(a, 50, i)).expect("a@50");
        }
        for i in 0..26i32 {
            shadow.apply_reorg_event(&mk(b, 51, i)).expect("b@51");
        }
        for i in 26..52i32 {
            shadow.apply_reorg_event(&mk(a, 52, i)).expect("a@52");
        }

        let pending = shadow.take_retier_pending();
        assert_eq!(
            pending.len(),
            2,
            "two distinct pools queued; A deduped across its two blocks"
        );
        let ids: Vec<PoolIdentifier> = pending.into_iter().map(|(_, id)| id).collect();
        assert!(ids.contains(&PoolIdentifier::Address(Address::from(a))));
        assert!(ids.contains(&PoolIdentifier::Address(Address::from(b))));

        let _ = std::fs::remove_file(&path);
    }
}
