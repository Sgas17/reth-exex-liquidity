// Unix Domain Socket Server for Pool Updates
//
// Sends pool state updates to connected orderbook engine clients

use crate::types::ControlMessage;
use eyre::Result;
use std::path::Path;
use tokio::{
    io::AsyncWriteExt,
    net::{UnixListener, UnixStream},
    sync::{broadcast, mpsc},
};
use tracing::{error, info, warn};

const SOCKET_PATH: &str = "/tmp/reth_exex_pool_updates.sock";
const BUFFER_SIZE: usize = 10_000; // Buffer up to 10k messages if client is slow

/// Bounded channel capacity between ExEx producer and socket broadcast loop.
/// 50k messages ≈ several thousand blocks worth of events. If exceeded, the
/// ExEx drops messages rather than accumulating unbounded memory.
const CHANNEL_CAPACITY: usize = 50_000;

/// Unix socket server that broadcasts pool updates to connected clients
pub struct PoolUpdateSocketServer {
    listener: UnixListener,
    message_tx: mpsc::Sender<ControlMessage>,
    message_rx: mpsc::Receiver<ControlMessage>,
    broadcast_tx: broadcast::Sender<ControlMessage>,
}

impl PoolUpdateSocketServer {
    /// Create a new socket server
    pub fn new() -> Result<Self> {
        // Remove existing socket if it exists
        let socket_path = Path::new(SOCKET_PATH);
        if socket_path.exists() {
            std::fs::remove_file(socket_path)?;
        }

        // Bind Unix socket
        let listener = UnixListener::bind(socket_path)?;

        // Set socket permissions to allow any user to connect
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::Permissions::from_mode(0o666);
            std::fs::set_permissions(socket_path, permissions)?;
        }

        info!("Unix socket server listening on {}", SOCKET_PATH);

        let (message_tx, message_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (broadcast_tx, _) = broadcast::channel(BUFFER_SIZE);

        Ok(Self {
            listener,
            message_tx,
            message_rx,
            broadcast_tx,
        })
    }

    /// Get a sender handle for publishing messages
    pub fn get_sender(&self) -> mpsc::Sender<ControlMessage> {
        self.message_tx.clone()
    }

    /// Run the server, accepting connections and broadcasting messages
    pub async fn run(mut self) -> Result<()> {
        info!("Pool update socket server starting");

        let broadcast_tx = self.broadcast_tx.clone();

        // Spawn task to accept new connections
        let listener = self.listener;
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        info!("New client connected to pool update socket");
                        let client_rx = broadcast_tx.subscribe();

                        // Spawn handler for this client
                        tokio::spawn(async move {
                            if let Err(e) = handle_client(stream, client_rx).await {
                                warn!("Client handler error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        error!("Failed to accept connection: {}", e);
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        });

        // Main broadcast loop - receive from message_rx and broadcast to all clients
        info!("Socket server broadcast loop starting");
        while let Some(message) = self.message_rx.recv().await {
            // Broadcast to all connected clients
            // Ignore errors - clients may disconnect
            let _ = self.broadcast_tx.send(message);
        }

        info!("Socket server shutting down");
        Ok(())
    }
}

/// Handle a single client connection
async fn handle_client(
    mut stream: UnixStream,
    mut broadcast_rx: broadcast::Receiver<ControlMessage>,
) -> Result<()> {
    // Receive messages from broadcast channel and send to this client
    loop {
        let message = match broadcast_rx.recv().await {
            Ok(msg) => msg,
            Err(broadcast::error::RecvError::Closed) => {
                info!("Broadcast channel closed");
                break;
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                warn!("Client lagged, skipped {} messages — disconnecting for resync", skipped);
                break;
            }
        };

        // Serialize message with bincode
        let serialized = match bincode::serialize(&message) {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to serialize message: {}", e);
                continue;
            }
        };

        // Send length prefix (4 bytes) + message
        let len = serialized.len() as u32;
        let len_bytes = len.to_le_bytes();

        if let Err(e) = stream.write_all(&len_bytes).await {
            error!("Failed to write message length: {}", e);
            break;
        }

        if let Err(e) = stream.write_all(&serialized).await {
            error!("Failed to write message: {}", e);
            break;
        }

        if let Err(e) = stream.flush().await {
            error!("Failed to flush stream: {}", e);
            break;
        }
    }

    info!("Client disconnected");
    Ok(())
}

/// Simple broadcaster that clones messages to all client channels
/// This is a simplified version - in production use tokio::sync::broadcast
pub struct MessageBroadcaster {
    clients: Vec<mpsc::Sender<ControlMessage>>,
    rx: mpsc::UnboundedReceiver<ControlMessage>,
}

impl MessageBroadcaster {
    pub fn new(rx: mpsc::UnboundedReceiver<ControlMessage>) -> Self {
        Self {
            clients: Vec::new(),
            rx,
        }
    }

    pub fn add_client(&mut self, client_tx: mpsc::Sender<ControlMessage>) {
        self.clients.push(client_tx);
    }

    pub async fn run(mut self) {
        while let Some(message) = self.rx.recv().await {
            // Broadcast to all clients
            let mut disconnected = Vec::new();

            for (idx, client) in self.clients.iter().enumerate() {
                if client.send(message.clone()).await.is_err() {
                    disconnected.push(idx);
                }
            }

            // Remove disconnected clients
            for idx in disconnected.into_iter().rev() {
                self.clients.remove(idx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_socket_creation() {
        let server = PoolUpdateSocketServer::new().unwrap();
        let sender = server.get_sender();

        // Should be able to get sender
        assert!(sender.is_closed() == false);

        // Cleanup
        let _ = std::fs::remove_file(SOCKET_PATH);
    }
}
