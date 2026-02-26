//! Append-only token tracking set with JSON persistence.
//!
//! Tokens are added when discovered via whitelist updates but never removed.
//! This ensures we don't lose track of a token (and its balance) if it gets
//! removed from the whitelist while the service is down.

use alloy_primitives::Address;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Tracks which tokens to monitor. Append-only — tokens are never removed.
pub struct TokenTracker {
    /// token address → decimals
    tokens: HashMap<Address, u8>,
    /// Path to JSON persistence file
    persist_path: PathBuf,
}

impl TokenTracker {
    /// Create a new tracker, loading persisted tokens from disk if the file exists.
    pub fn new(persist_path: PathBuf) -> Self {
        let tokens = load_from_disk(&persist_path).unwrap_or_default();
        if !tokens.is_empty() {
            info!(count = tokens.len(), path = %persist_path.display(), "loaded persisted token set");
        }
        Self {
            tokens,
            persist_path,
        }
    }

    /// Add a token to the tracking set. Returns true if the token was new.
    pub fn add(&mut self, token: Address, decimals: u8) -> bool {
        if self.tokens.contains_key(&token) {
            return false;
        }
        self.tokens.insert(token, decimals);
        if let Err(e) = save_to_disk(&self.persist_path, &self.tokens) {
            warn!(error = %e, "failed to persist token set");
        }
        true
    }

    /// Check if a token is being tracked.
    pub fn contains(&self, token: &Address) -> bool {
        self.tokens.contains_key(token)
    }

    /// Get the decimals for a tracked token.
    pub fn decimals(&self, token: &Address) -> Option<u8> {
        self.tokens.get(token).copied()
    }

    /// Iterate over all tracked tokens.
    pub fn iter(&self) -> impl Iterator<Item = (&Address, &u8)> {
        self.tokens.iter()
    }

    /// Number of tracked tokens.
    pub fn len(&self) -> usize {
        self.tokens.len()
    }
}

/// JSON format: `{ "0xaddr": decimals, ... }`
fn load_from_disk(path: &Path) -> Option<HashMap<Address, u8>> {
    let content = std::fs::read_to_string(path).ok()?;
    let raw: HashMap<String, u8> = serde_json::from_str(&content).ok()?;
    let mut tokens = HashMap::new();
    for (addr_str, decimals) in raw {
        if let Ok(addr) = addr_str.parse::<Address>() {
            tokens.insert(addr, decimals);
        } else {
            warn!(address = %addr_str, "skipping invalid address in persisted token set");
        }
    }
    Some(tokens)
}

/// Atomic write: serialize → write to `.tmp` → rename over target.
/// `rename` is atomic on POSIX when src and dst are on the same filesystem
/// (guaranteed here since they share the same parent directory).
fn save_to_disk(path: &Path, tokens: &HashMap<Address, u8>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
    }
    let raw: HashMap<String, u8> = tokens
        .iter()
        .map(|(addr, dec)| (format!("{addr:#x}"), *dec))
        .collect();
    let json = serde_json::to_string_pretty(&raw).map_err(|e| format!("serialize: {e}"))?;

    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, &json).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp_path, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;
    use std::io::Write;

    #[test]
    fn add_returns_true_for_new_token() {
        let tmp = tempfile();
        let mut tracker = TokenTracker::new(tmp.clone());
        let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        assert!(tracker.add(usdc, 6));
        assert!(!tracker.add(usdc, 6)); // duplicate
        assert!(tracker.contains(&usdc));
        assert_eq!(tracker.decimals(&usdc), Some(6));
    }

    #[test]
    fn persistence_roundtrip() {
        let tmp = tempfile();
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");

        {
            let mut tracker = TokenTracker::new(tmp.clone());
            tracker.add(weth, 18);
            assert_eq!(tracker.len(), 1);
        }

        // Re-load
        let tracker = TokenTracker::new(tmp);
        assert!(tracker.contains(&weth));
        assert_eq!(tracker.decimals(&weth), Some(18));
    }

    #[test]
    fn loads_empty_if_no_file() {
        let tracker = TokenTracker::new(PathBuf::from("/tmp/nonexistent_test_balance_tokens.json"));
        assert_eq!(tracker.len(), 0);
    }

    fn tempfile() -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "balance_monitor_test_{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        path
    }
}
