use criterion::{black_box, criterion_group, criterion_main, Criterion};
use alloy_primitives::U256;
use reth_exex_liquidity::fluid_decoder::{
    decode_fluid_reserves, FluidPoolConfig, FluidStorageSlots,
};

/// Pool 1 (wstETH/ETH) storage slots captured from mainnet.
fn pool1_slots() -> FluidStorageSlots {
    FluidStorageSlots {
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
            "0200000000000007c80dd0e890000007904d82ac31a6d0089c01e52543e80015",
            16,
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
    }
}

fn pool1_config() -> FluidPoolConfig {
    FluidPoolConfig {
        token0_numerator_precision: 1,
        token0_denominator_precision: 1_000_000,
        token1_numerator_precision: 1,
        token1_denominator_precision: 1_000_000,
    }
}

fn bench_decode(c: &mut Criterion) {
    let slots = pool1_slots();
    let config = pool1_config();
    let timestamp = 1773437867u64;

    c.bench_function("decode_fluid_reserves", |b| {
        b.iter(|| {
            black_box(decode_fluid_reserves(
                black_box(&slots),
                black_box(&config),
                black_box(timestamp),
            ))
        })
    });
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
