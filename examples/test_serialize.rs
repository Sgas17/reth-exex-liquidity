use alloy_primitives::{Address, I256, U256};
use bincode;

fn main() {
    println!("Testing bincode serialization for alloy_primitives types\n");
    println!("{}", "=".repeat(80));

    // Test U256
    let u256_value = U256::from(1382840672037684546977487336313952u128);
    let u256_bytes = bincode::serialize(&u256_value).unwrap();
    println!("U256 value: {}", u256_value);
    println!("U256 bytes length: {}", u256_bytes.len());
    println!("U256 bytes (hex): {}", hex::encode(&u256_bytes));
    println!();

    // Test small U256
    let u256_small = U256::from(32u64);
    let u256_small_bytes = bincode::serialize(&u256_small).unwrap();
    println!("U256 small value: {}", u256_small);
    println!("U256 small bytes length: {}", u256_small_bytes.len());
    println!("U256 small bytes (hex): {}", hex::encode(&u256_small_bytes));
    println!();

    // Test I256 positive
    let i256_pos = I256::try_from(1512537406709823118i128).unwrap();
    let i256_pos_bytes = bincode::serialize(&i256_pos).unwrap();
    println!("I256 positive value: {}", i256_pos);
    println!("I256 positive bytes length: {}", i256_pos_bytes.len());
    println!(
        "I256 positive bytes (hex): {}",
        hex::encode(&i256_pos_bytes)
    );
    println!();

    // Test I256 negative
    let i256_neg = I256::try_from(-4965441256i64).unwrap();
    let i256_neg_bytes = bincode::serialize(&i256_neg).unwrap();
    println!("I256 negative value: {}", i256_neg);
    println!("I256 negative bytes length: {}", i256_neg_bytes.len());
    println!(
        "I256 negative bytes (hex): {}",
        hex::encode(&i256_neg_bytes)
    );
    println!();

    // Test Address
    let addr = "0x8ad599c3a0ff1de082011efddc58f1908eb6e6d8"
        .parse::<Address>()
        .unwrap();
    let addr_bytes = bincode::serialize(&addr).unwrap();
    println!("Address value: {:?}", addr);
    println!("Address bytes length: {}", addr_bytes.len());
    println!("Address bytes (hex): {}", hex::encode(&addr_bytes));
    println!();

    // Test u128
    let u128_val = 3100233156779584315u128;
    let u128_bytes = bincode::serialize(&u128_val).unwrap();
    println!("u128 value: {}", u128_val);
    println!("u128 bytes length: {}", u128_bytes.len());
    println!("u128 bytes (hex): {}", hex::encode(&u128_bytes));
    println!();

    // Test i32
    let i32_val = 195356i32;
    let i32_bytes = bincode::serialize(&i32_val).unwrap();
    println!("i32 value: {}", i32_val);
    println!("i32 bytes length: {}", i32_bytes.len());
    println!("i32 bytes (hex): {}", hex::encode(&i32_bytes));
}
