//! ExEx-side pool-arena writer (ITE-16, step 2).
//!
//! This is the future home of the in-process arena writer: ExEx becomes the
//! sole writer of the `/dev/shm/pool_arena` shared memory, replacing the
//! `ExEx -> socket -> arena_service` replication path.
//!
//! The mmap **layout** lives in the shared [`arena_layout`] crate and the
//! **writer** (slot allocation + typed write API + mmap open/create) lives in
//! the shared [`arena_writer`] crate — both also used by `arena_service`, so the
//! two writers are the same code driven by different inputs (socket events vs.
//! canonical chain notifications).
//!
//! Next: drive [`arena_writer::SharedArenaWriter`] from the ExEx notification
//! loop behind a flag (shadow arena), then startup hydration and reorg writes.

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;
    use arena_layout::{SharedArenaRegion, UniswapV3LowPoolData, SHARED_ARENA_VERSION};
    use arena_writer::{ArenaMmap, SharedArenaWriter};

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
    /// shared `arena_writer::SharedArenaWriter` — the primitives the shadow
    /// writer will use.
    #[test]
    fn exex_writes_arena_via_shared_writer() {
        let path =
            std::env::temp_dir().join(format!("ite16_shadow_{}.arena", std::process::id()));

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
}
