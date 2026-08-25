//! AeroExecute-Engine — Lock-Free Order Queue (Rust)
//!
//! Ultra-low-latency signal transport using a bounded asynchronous channel.
//! Designed to keep the critical path under 20 µs by eliminating locks and
//! minimising allocations.
//!
//! In production this would sit on top of a pre-allocated SPSC ring buffer
//! (e.g. crossbeam or a custom cache-line-padded ring). The channel-based
//! version shown here preserves the same ownership and back-pressure semantics.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use thiserror::Error;

/// Hard latency budget for a single enqueue (documentation contract).
pub const ENQUEUE_BUDGET_US: u64 = 20;

#[derive(Debug, Clone)]
pub struct TradeSignal {
    pub symbol: String,
    pub side: Side,
    pub quantity: f64,
    pub limit_price: Option<f64>,
    pub client_tag: u64,
    pub consensus_score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Error)]
pub enum QueueError {
    #[error("queue closed")]
    Closed,
    #[error("queue full — back-pressure applied")]
    Full,
    #[error("invalid quantity: {0}")]
    InvalidQuantity(f64),
    #[error("consensus below threshold")]
    LowConsensus,
}

/// Lock-free oriented order queue.
///
/// - Bounded capacity → natural back-pressure
/// - Signals are moved (not cloned) into the channel
/// - Counters are atomic for cross-thread observability
pub struct OrderQueue {
    tx: mpsc::Sender<TradeSignal>,
    enqueued: Arc<AtomicU64>,
    rejected: Arc<AtomicU64>,
}

impl OrderQueue {
    /// Create a new queue with a fixed capacity.
    /// Returns the producer handle and the consumer receiver.
    pub fn new(capacity: usize) -> (Self, mpsc::Receiver<TradeSignal>) {
        let (tx, rx) = mpsc::channel(capacity);
        let queue = Self {
            tx,
            enqueued: Arc::new(AtomicU64::new(0)),
            rejected: Arc::new(AtomicU64::new(0)),
        };
        (queue, rx)
    }

    /// Attempt to enqueue a signal under the documented latency budget.
    /// Fails closed on validation or capacity errors.
    pub async fn enqueue(&self, signal: TradeSignal) -> Result<(), QueueError> {
        if signal.quantity <= 0.0 {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(QueueError::InvalidQuantity(signal.quantity));
        }
        if signal.consensus_score < 0.92 {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(QueueError::LowConsensus);
        }

        // Non-blocking try first to preserve latency when the consumer is healthy.
        match self.tx.try_send(signal) {
            Ok(()) => {
                self.enqueued.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                Err(QueueError::Full)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                Err(QueueError::Closed)
            }
        }
    }

    /// Blocking variant for callers that already run inside a Tokio context
    /// and are willing to await under back-pressure.
    pub async fn enqueue_async(&self, signal: TradeSignal) -> Result<(), QueueError> {
        if signal.quantity <= 0.0 {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(QueueError::InvalidQuantity(signal.quantity));
        }
        if signal.consensus_score < 0.92 {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(QueueError::LowConsensus);
        }

        self.tx
            .send(signal)
            .await
            .map_err(|_| QueueError::Closed)?;
        self.enqueued.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn enqueued_count(&self) -> u64 {
        self.enqueued.load(Ordering::Relaxed)
    }

    pub fn rejected_count(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_signal(qty: f64, score: f64) -> TradeSignal {
        TradeSignal {
            symbol: "EURUSD".into(),
            side: Side::Buy,
            quantity: qty,
            limit_price: Some(1.0850),
            client_tag: 7,
            consensus_score: score,
        }
    }

    #[tokio::test]
    async fn enqueue_accepts_valid_signal() {
        let (q, mut rx) = OrderQueue::new(16);
        q.enqueue(sample_signal(1.0, 0.95)).await.unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(received.client_tag, 7);
        assert_eq!(q.enqueued_count(), 1);
    }

    #[tokio::test]
    async fn enqueue_rejects_low_consensus() {
        let (q, _rx) = OrderQueue::new(8);
        let err = q.enqueue(sample_signal(1.0, 0.85)).await.unwrap_err();
        assert!(matches!(err, QueueError::LowConsensus));
        assert_eq!(q.rejected_count(), 1);
    }

    #[tokio::test]
    async fn enqueue_rejects_zero_quantity() {
        let (q, _rx) = OrderQueue::new(8);
        let err = q.enqueue(sample_signal(0.0, 0.99)).await.unwrap_err();
        assert!(matches!(err, QueueError::InvalidQuantity(_)));
    }
      }
