//! Remote bridge — WebSocket-outbound connection to a relay server.
//!
//! The bridge subscribes to daemon events, forwards them over WebSocket to a remote relay,
//! receives inbound commands from the relay, and automatically reconnects with exponential
//! backoff on disconnection.

use async_trait::async_trait;
use thiserror::Error;

/// Errors raised during bridge operation.
#[derive(Debug, Clone, Error)]
pub enum BridgeError {
    #[error("transport disconnected")]
    Disconnected,
    #[error("io error: {0}")]
    Io(String),
}

/// A trait abstracting the underlying transport (WebSocket, mock channel, etc.).
#[async_trait]
pub trait BridgeTransport: Send + Sync {
    /// Establish a connection to the remote relay.
    async fn connect(&self) -> Result<(), BridgeError>;

    /// Send a message to the relay.
    async fn send(&self, msg: String) -> Result<(), BridgeError>;

    /// Receive a message from the relay; returns None if EOF (connection closed gracefully).
    async fn recv(&self) -> Result<Option<String>, BridgeError>;
}

/// A WebSocket-outbound bridge from the daemon to a remote relay.
///
/// The bridge runs an event loop that:
/// - Forwards outbound messages (from the daemon) to the transport
/// - Reads inbound messages from the transport and forwards them via the inbound channel
/// - Automatically reconnects with exponential backoff on I/O error
pub struct RemoteBridge<T: BridgeTransport> {
    transport: T,
    backoff_ms: u64,
    max_backoff_ms: u64,
}

impl<T: BridgeTransport> RemoteBridge<T> {
    /// Create a new bridge with the given transport and default backoff limits.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            backoff_ms: 100,
            max_backoff_ms: 30000,
        }
    }

    /// Create a bridge with custom backoff limits.
    pub fn with_backoff(transport: T, max_backoff_ms: u64) -> Self {
        Self {
            transport,
            backoff_ms: 100,
            max_backoff_ms,
        }
    }

    /// Run the bridge's event loop.
    ///
    /// Accepts:
    /// - `outbound`: a channel receiving messages from the daemon to forward to the relay
    /// - `inbound`: a channel to send relay messages to for daemon consumption
    ///
    /// This loops until the outbound channel is dropped. On any transport error, it waits
    /// (with exponential backoff) and reconnects.
    pub async fn run(
        &mut self,
        mut outbound: tokio::sync::mpsc::Receiver<String>,
        inbound: tokio::sync::mpsc::Sender<String>,
    ) -> BridgeError {
        loop {
            match self.connect_and_forward(&mut outbound, &inbound).await {
                Ok(_) => break,
                Err(BridgeError::Disconnected) => {
                    log_bridge_error(&BridgeError::Disconnected);
                    let wait_ms = self.next_backoff();
                    tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms)).await;
                }
                Err(e) => {
                    log_bridge_error(&e);
                    let wait_ms = self.next_backoff();
                    tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms)).await;
                }
            }
        }
        BridgeError::Disconnected
    }

    /// Attempt a single connection and forward in both directions until error or EOF.
    async fn connect_and_forward(
        &mut self,
        outbound: &mut tokio::sync::mpsc::Receiver<String>,
        inbound: &tokio::sync::mpsc::Sender<String>,
    ) -> Result<(), BridgeError> {
        self.transport.connect().await?;
        self.backoff_ms = 100;

        loop {
            tokio::select! {
                msg = outbound.recv() => {
                    match msg {
                        Some(m) => self.transport.send(m).await?,
                        None => return Ok(()),
                    }
                }
                recv_result = self.transport.recv() => {
                    match recv_result? {
                        Some(m) => {
                            let _ = inbound.send(m).await;
                        }
                        None => return Err(BridgeError::Disconnected),
                    }
                }
            }
        }
    }

    /// Calculate the next backoff duration, doubling until max.
    fn next_backoff(&mut self) -> u64 {
        let current = self.backoff_ms;
        self.backoff_ms = std::cmp::min(self.backoff_ms * 2, self.max_backoff_ms);
        current
    }
}

fn log_bridge_error(e: &BridgeError) {
    eprintln!("[bridge] error: {}", e);
}
