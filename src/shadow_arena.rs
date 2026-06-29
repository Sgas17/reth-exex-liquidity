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
//! Sub-step 3a (this commit): block-boundary plumbing only — open the arena and
//! signal each block. Slot creation (topology) needs pool decimals from
//! scraping and lands with startup hydration (3b); live per-block apply lands in
//! 3c; reorg writes in 3d.

use arena_layout::SIGNAL_REASON_LIVE_BLOCK_EMPTY;
use arena_writer::ArenaMmap;
use std::path::{Path, PathBuf};

/// Env var naming the shadow arena mmap path. When unset, the shadow writer is
/// disabled.
pub const SHADOW_ARENA_PATH_ENV: &str = "SHADOW_ARENA_PATH";

/// In-process writer for the (shadow) pool arena.
pub struct ShadowArena {
    arena: ArenaMmap,
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
            "Shadow arena opened (ITE-16 3a: block-signal plumbing; topology + apply pending)"
        );
        Ok(Self { arena })
    }

    /// Block boundary end. 3a: signal an empty block so a reader can confirm the
    /// shadow arena advances per block. Live per-block apply lands in 3c.
    pub fn end_block(&mut self, block_number: u64) {
        self.arena.region().header.signal_update_complete(
            block_number,
            0,
            SIGNAL_REASON_LIVE_BLOCK_EMPTY,
            0,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use arena_layout::{SharedArenaRegion, UniswapV3LowPoolData, SHARED_ARENA_VERSION};
    use arena_writer::SharedArenaWriter;

    fn temp_arena_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ite16_{tag}_{}.arena", std::process::id()))
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
}
