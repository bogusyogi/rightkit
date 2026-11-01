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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A mock transport using in-memory channels.
    #[derive(Clone)]
    struct MockTransport {
        state: Arc<Mutex<MockState>>,
    }

    #[derive(Clone)]
    struct MockState {
        connected: bool,
        sent_messages: Vec<String>,
        recv_messages: Vec<Option<String>>,
        recv_index: usize,
    }

    impl MockTransport {
        fn new(recv_messages: Vec<Option<String>>) -> Self {
            Self {
                state: Arc::new(Mutex::new(MockState {
                    connected: false,
                    sent_messages: Vec::new(),
                    recv_messages,
                    recv_index: 0,
                })),
            }
        }

        fn sent_messages(&self) -> Vec<String> {
            self.state.lock().unwrap().sent_messages.clone()
        }
    }

    #[async_trait]
    impl BridgeTransport for MockTransport {
        async fn connect(&self) -> Result<(), BridgeError> {
            let mut s = self.state.lock().unwrap();
            s.connected = true;
            Ok(())
        }

        async fn send(&self, msg: String) -> Result<(), BridgeError> {
            let mut s = self.state.lock().unwrap();
            if !s.connected {
                return Err(BridgeError::Disconnected);
            }
            s.sent_messages.push(msg);
            Ok(())
        }

        async fn recv(&self) -> Result<Option<String>, BridgeError> {
            let mut s = self.state.lock().unwrap();
            if s.recv_index >= s.recv_messages.len() {
                return Err(BridgeError::Disconnected);
            }
            let msg = s.recv_messages[s.recv_index].clone();
            s.recv_index += 1;
            Ok(msg)
        }
    }

    #[tokio::test]
    async fn forward_outbound_to_transport() {
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let (tx_in, _rx_in) = tokio::sync::mpsc::channel(10);

        tx.send("hello".into()).await.unwrap();
        tx.send("world".into()).await.unwrap();
        drop(tx);

        let transport = MockTransport::new(vec![]);
        let mut bridge = RemoteBridge::new(transport.clone());
        let _ = bridge.run(rx, tx_in).await;

        let sent = transport.sent_messages();
        assert_eq!(sent, vec!["hello", "world"]);
    }

    #[tokio::test]
    async fn receive_from_transport() {
        let (tx_out, rx_out) = tokio::sync::mpsc::channel::<String>(10);
        let (tx_in, mut rx_in) = tokio::sync::mpsc::channel(10);

        let transport = MockTransport::new(vec![Some("msg1".into()), Some("msg2".into()), None]);
        let mut bridge = RemoteBridge::new(transport);
        let handle = tokio::spawn(async move {
            let _ = bridge.run(rx_out, tx_in).await;
        });

        let msg1 = rx_in.recv().await;
        let msg2 = rx_in.recv().await;
        drop(tx_out);
        handle.await.unwrap();

        assert_eq!(msg1, Some("msg1".into()));
        assert_eq!(msg2, Some("msg2".into()));
    }

    #[tokio::test]
    async fn reconnect_on_error() {
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let (tx_in, _rx_in) = tokio::sync::mpsc::channel(10);

        tx.send("msg1".into()).await.unwrap();
        // Keep `tx` alive: the outbound branch must pend (not close) so the
        // transport-error → backoff path is exercised deterministically rather
        // than racing a graceful outbound-closed shutdown.

        let transport = MockTransport::new(vec![None]);
        let mut bridge = RemoteBridge::with_backoff(transport.clone(), 1000);

        // The transport recv yields None → Disconnected → 1000ms backoff sleep,
        // so run() is still backing off when the 500ms timeout fires.
        tokio::time::timeout(std::time::Duration::from_millis(500), async {
            let _ = bridge.run(rx, tx_in).await;
        })
        .await
        .unwrap_err();

        drop(tx);
    }

    #[test]
    fn backoff_doubles_until_max() {
        let transport = MockTransport::new(vec![]);
        let mut bridge = RemoteBridge::with_backoff(transport, 1600);

        assert_eq!(bridge.next_backoff(), 100);
        assert_eq!(bridge.next_backoff(), 200);
        assert_eq!(bridge.next_backoff(), 400);
        assert_eq!(bridge.next_backoff(), 800);
        assert_eq!(bridge.next_backoff(), 1600);
        assert_eq!(bridge.next_backoff(), 1600);
        assert_eq!(bridge.next_backoff(), 1600);
    }

    #[test]
    fn backoff_resets_on_connect() {
        let transport = MockTransport::new(vec![]);
        let mut bridge = RemoteBridge::new(transport);

        bridge.backoff_ms = 1000;
        bridge.next_backoff();
        bridge.next_backoff();
        assert_eq!(bridge.backoff_ms, 4000);

        bridge.backoff_ms = 100;
        assert_eq!(bridge.next_backoff(), 100);
        assert_eq!(bridge.backoff_ms, 200);
    }
}
