use alloy_primitives::{Address, Log, U256};
use alloy_sol_types::{sol, SolEvent};

sol! {
    #[derive(Debug)]
    event Transfer(address indexed from, address indexed to, uint256 value);
}

pub struct DecodedTransfer {
    pub token: Address,
    pub from: Address,
    pub to: Address,
    pub value: U256,
}

/// Decode a log as an ERC20 Transfer. Returns None if not a Transfer event.
///
/// ERC721 also emits Transfer(address,address,uint256) but with tokenId indexed
/// (4 topics vs 3), so alloy's decode_log rejects those automatically.
pub fn decode_transfer(log: &Log) -> Option<DecodedTransfer> {
    let topic0 = log.topics().first()?;
    if topic0.0 != Transfer::SIGNATURE_HASH.0 {
        return None;
    }

    let decoded = Transfer::decode_log(log).ok()?;

    Some(DecodedTransfer {
        token: log.address,
        from: decoded.data.from,
        to: decoded.data.to,
        value: decoded.data.value,
    })
}
