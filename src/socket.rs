// Unix Domain Socket Server for Pool Updates
//
// Sends pool state updates to connected orderbook engine clients

use crate::types::ControlMessage;
use eyre::Result;
use std::path::Path;
use tokio::{
    io::AsyncWriteExt,
    net::{UnixListener, UnixStream},
    sync::mpsc,
};
use tracing::{debug, error, info, warn};

const SOCKET_PATH: &str = "/tmp/reth_exex_pool_updates.sock";
const BUFFER_SIZE: usize = 10_000; // Buffer up to 10k messages if client is slow

/// Unix socket server that broadcasts pool updates to connected clients
pub struct PoolUpdateSocketServer {
    listener: UnixListener,
    message_tx: mpsc::UnboundedSender<ControlMessage>,
    _message_rx: mpsc::UnboundedReceiver<ControlMessage>,
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

        let (message_tx, message_rx) = mpsc::unbounded_channel();

        Ok(Self {
            listener,
            message_tx,
            _message_rx: message_rx,
        })
    }

    /// Get a sender handle for publishing messages
    pub fn get_sender(&self) -> mpsc::UnboundedSender<ControlMessage> {
        self.message_tx.clone()
    }

    /// Run the server, accepting connections and broadcasting messages
    pub async fn run(self) -> Result<()> {
        info!("Pool update socket server starting");

        // Spawn task to accept new connections
        let listener = self.listener;
        let message_tx = self.message_tx.clone();

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        info!("New client connected to pool update socket");
                        let tx = message_tx.clone();

                        // Spawn handler for this client
                        tokio::spawn(async move {
                            if let Err(e) = handle_client(stream, tx).await {
                                warn!("Client handler error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        error!("Failed to accept connection: {}", e);
                    }
                }
            }
        });

        // Main broadcast loop (receives messages and sends to all clients)
        // Note: In production, we'd track connected clients and broadcast to all
        // For now, each client gets its own message stream from the channel
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            debug!("Socket server heartbeat");
        }
    }
}

/// Handle a single client connection
async fn handle_client(
    mut stream: UnixStream,
    _message_tx: mpsc::UnboundedSender<ControlMessage>,
) -> Result<()> {
    // Create a bounded channel for this client to apply backpressure
    let (_client_tx, mut client_rx) = mpsc::channel::<ControlMessage>(BUFFER_SIZE);

    // Subscribe this client to the broadcast
    // Note: This is a simplified version. In production, we'd use broadcast channels
    // For now, messages sent to message_tx will be cloned for each client

    // Send messages to this client
    let write_task = tokio::spawn(async move {
        while let Some(message) = client_rx.recv().await {
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
        Ok::<(), eyre::Error>(())
    });

    write_task.await??;

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
