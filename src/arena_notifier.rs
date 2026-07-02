//! Arena → Curve notification sender (ITE-20 production cutover).
//!
//! When the ExEx is the sole, authoritative writer of the pool arena
//! (`SHARED_ARENA_PATH` set), it emits the per-block [`ArenaBlockNotification`]
//! directly — the role `arena_service` performed before the cutover. This is the
//! sender half only; the curve-side receiver lives in
//! `pool_state_arena::notifier::ArenaNotificationReceiver` and is unchanged.
//!
//! Wire format is identical to `arena_service`'s notifier so `curve_service`
//! connects to the same socket path with no changes: `[4-byte LE length]` then a
//! bincode-encoded [`ArenaBlockNotification`]. The notification type is the
//! shared [`arena_layout`] contract, so both writers serialize byte-for-byte.

use arena_layout::ArenaBlockNotification;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

/// Default socket path for arena → curve notifications. Must match
/// `pool_state_arena::notifier::ARENA_NOTIFY_SOCKET` (the curve default).
pub const ARENA_NOTIFY_SOCKET: &str = "/tmp/arena_curve_notify.sock";

/// ExEx-side arena → curve notifier. Binds the notification socket and streams
/// block notifications to the connected `curve_service`. Never blocks the ExEx:
/// with no client, or on a write error, it drops the notification and continues.
pub struct ArenaCurveNotifier {
    /// Current connected client (if any).
    client: Option<UnixStream>,
    /// Receives newly-accepted client streams from the background accept task.
    accept_rx: mpsc::Receiver<UnixStream>,
    path: PathBuf,
}

impl ArenaCurveNotifier {
    /// Bind the notification socket at `path` and spawn the background accept
    /// task. Removes any stale socket file first.
    pub fn bind(path: &str) -> std::io::Result<Self> {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        // The ExEx runs in a container as its own uid; host-side curve_service
        // must still be able to connect (mirrors the pool-update socket).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
        }
        tracing::info!(path = %path, "ExEx arena → curve notifier listening");

        let (accept_tx, accept_rx) = mpsc::channel(2);

        // Background task: accept connections and hand them to the main loop.
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        tracing::info!("curve_service connected to arena notification socket");
                        if accept_tx.send(stream).await.is_err() {
                            break; // notifier dropped
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Notification socket accept error");
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            }
        });

        Ok(Self {
            client: None,
            accept_rx,
            path: PathBuf::from(path),
        })
    }

    /// Send a block notification to the connected curve service.
    ///
    /// If no client is connected or the write fails, logs and continues — the
    /// ExEx must never block on (or fail because of) the curve service.
    pub async fn notify(&mut self, notification: &ArenaBlockNotification) {
        // Adopt the latest accepted client (a reconnect supersedes the old one).
        while let Ok(stream) = self.accept_rx.try_recv() {
            self.client = Some(stream);
        }

        let Some(stream) = self.client.as_mut() else {
            return;
        };

        let payload = match bincode::serialize(notification) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "Failed to serialize arena notification");
                return;
            }
        };

        let len = (payload.len() as u32).to_le_bytes();
        let result = async {
            stream.write_all(&len).await?;
            stream.write_all(&payload).await?;
            stream.flush().await
        }
        .await;

        if let Err(e) = result {
            tracing::warn!(error = %e, "Failed to send notification to curve service");
            self.client = None;
        }
    }
}

impl Drop for ArenaCurveNotifier {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_layout::PoolIdentifier;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;

    /// The ExEx notifier must emit the exact `[4-byte LE length][bincode]` frame
    /// the curve-side receiver decodes, using the shared `arena_layout` type — so
    /// `curve_service` reads it byte-for-byte the same as arena_service's frames.
    #[tokio::test]
    async fn notify_round_trips_over_socket() {
        let path = format!("/tmp/ite20_arena_notify_test_{}.sock", std::process::id());
        let mut notifier = ArenaCurveNotifier::bind(&path).expect("bind notifier");

        // Connect a client; let the background accept task hand the stream over.
        let mut client = UnixStream::connect(&path).await.expect("connect client");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let notification = ArenaBlockNotification {
            block_number: 1234,
            end_stream_seq: 42,
            signal_reason: "live_block_apply".to_string(),
            updated_pools: vec![PoolIdentifier::Address([7u8; 20])],
            base_fee_per_gas: 99,
        };
        notifier.notify(&notification).await;

        // Decode the length-prefixed bincode frame the curve receiver expects.
        let mut len_buf = [0u8; 4];
        client.read_exact(&mut len_buf).await.expect("read length");
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut payload = vec![0u8; len];
        client.read_exact(&mut payload).await.expect("read payload");
        let decoded: ArenaBlockNotification =
            bincode::deserialize(&payload).expect("decode notification");

        assert_eq!(decoded.block_number, 1234);
        assert_eq!(decoded.end_stream_seq, 42);
        assert_eq!(decoded.signal_reason, "live_block_apply");
        assert_eq!(
            decoded.updated_pools,
            vec![PoolIdentifier::Address([7u8; 20])]
        );
        assert_eq!(decoded.base_fee_per_gas, 99);

        let _ = std::fs::remove_file(&path);
    }
}
