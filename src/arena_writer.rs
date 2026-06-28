//! ExEx-side pool-arena writer (ITE-16, step 2).
//!
//! This is the future home of the in-process arena writer: ExEx becomes the
//! sole writer of the `/dev/shm/pool_arena` shared memory, replacing the
//! `ExEx -> socket -> arena_service` replication path. The byte layout is owned
//! by the shared [`arena_layout`] crate, which both this writer and the
//! downstream readers (`curve_service`, …) compile against — one definition, no
//! drift.
//!
//! Right now this module only establishes the cross-repo dependency wiring; the
//! mmap open/create, slot allocation, startup hydration, live apply, and reorg
//! handling land in subsequent commits.

#[cfg(test)]
mod tests {
    //! Cross-repo wiring smoke test.
    //!
    //! Proves that `arena_layout` compiles into the ExEx (reth v2.2.0) build and,
    //! critically, that this crate's `alloy_primitives::U256` unifies with the
    //! `U256` used in `arena_layout`'s `#[repr(C)]` pool-data fields — i.e. both
    //! repos resolve to a single `alloy-primitives` version. If they did not,
    //! the assignment below would be a type error.

    use alloy_primitives::U256;
    use arena_layout::{SharedArenaRegion, UniswapV3LowPoolData, SHARED_ARENA_VERSION};

    #[test]
    fn arena_layout_types_are_usable_from_exex() {
        // U256 type unification across the crate boundary.
        let mut pool = UniswapV3LowPoolData::default();
        pool.sqrt_price_x96 = U256::from(123_456_u64);
        assert_eq!(pool.sqrt_price_x96, U256::from(123_456_u64));

        // Layout surface is reachable (version const + region sizing).
        assert_eq!(SHARED_ARENA_VERSION, 5);
        assert!(SharedArenaRegion::size() > 0);
    }
}
