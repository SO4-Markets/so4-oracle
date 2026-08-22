use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::SystemTime;

use serde::Serialize;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::metrics::Metrics;

pub const FAILURE_RING_CAPACITY: usize = 256;

#[derive(Debug, Clone, Serialize)]
pub struct CachedPrice {
    pub token_address: String,
    pub symbol: String,
    pub display_symbol: String,
    pub keeper_index: u32,
    #[serde(serialize_with = "ser_i128_str")]
    pub min: i128,
    #[serde(serialize_with = "ser_i128_str")]
    pub max: i128,
    #[serde(serialize_with = "ser_i128_str")]
    pub median: i128,
    pub timestamp: u64,
    #[serde(rename = "ledger")]
    pub ledger_seq: u32,
    pub sources_used: Vec<String>,
    pub signature: String,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct PriceCache {
    pub prices: BTreeMap<String, CachedPrice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<SystemTime>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct CycleStatus {
    pub price_cycle_running: bool,
    pub keeper_cycle_running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_price_cycle_at: Option<SystemTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_keeper_cycle_at: Option<SystemTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_keeper_cycle_latency_ms: Option<u64>,
}

/// Keeper executions retained for the admin endpoint. Unchanged from the manual trim this
/// replaced; the endpoint still returns the most recent 50 of them.
pub const KEEPER_EXECUTION_RING_CAPACITY: usize = 100;

#[derive(Debug, Clone, Serialize)]
pub struct KeeperStatus {
    pub pending_orders: usize,
    pub pending_deposits: usize,
    pub pending_withdrawals: usize,
    pub last_executions: RingBuffer<KeeperExecution>,
}

// Hand-written rather than derived: `RingBuffer::default()` is FAILURE_RING_CAPACITY, which is the
// failures list's 256, not this list's 100.
impl Default for KeeperStatus {
    fn default() -> Self {
        Self {
            pending_orders: 0,
            pending_deposits: 0,
            pending_withdrawals: 0,
            last_executions: RingBuffer::new(KEEPER_EXECUTION_RING_CAPACITY),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct KeeperExecution {
    pub timestamp: SystemTime,
    pub operation: String,
    pub key: String,
    pub tx_hash: Option<String>,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailedSubmission {
    pub at: SystemTime,
    pub operation: String,
    pub network: String,
    pub token: String,
    pub symbol: String,
    #[serde(serialize_with = "ser_i128_str")]
    pub min: i128,
    #[serde(serialize_with = "ser_i128_str")]
    pub max: i128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<String>,
    pub error: String,
    pub timestamp: u64,
    pub ledger_seq: u32,
}

pub fn ser_i128_str<S>(value: &i128, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&value.to_string())
}

#[derive(Debug, Clone)]
pub struct RingBuffer<T> {
    capacity: usize,
    items: VecDeque<T>,
}

impl<T> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            items: VecDeque::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, item: T) {
        if self.capacity == 0 {
            return;
        }
        while self.items.len() >= self.capacity {
            self.items.pop_front();
        }
        self.items.push_back(item);
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> {
        self.items.iter()
    }
}

impl<T> Default for RingBuffer<T> {
    fn default() -> Self {
        Self::new(FAILURE_RING_CAPACITY)
    }
}

// Oldest first, like the `Vec` this replaced, so anything serializing a struct that holds one sees
// the same JSON array it saw before.
impl<T: Serialize> Serialize for RingBuffer<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_seq(self.items.iter())
    }
}

#[derive(Debug, Default, Clone)]
pub struct ReadyCache {
    pub last_checked: Option<std::time::Instant>,
    pub last_error: Option<(axum::http::StatusCode, String)>,
}

/// Number of consecutive `freeze_order` failures before a key is permanently
/// skipped and a loud alert is emitted (#498).
pub const MAX_CONSECUTIVE_FREEZE_FAILURES: u32 = 3;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub http: reqwest::Client,
    pub price_cache: Arc<RwLock<PriceCache>>,
    pub cycle_status: Arc<RwLock<CycleStatus>>,
    pub failures: Arc<Mutex<RingBuffer<FailedSubmission>>>,
    pub keeper_status: Arc<RwLock<KeeperStatus>>,
    pub in_flight_keys: Arc<Mutex<std::collections::HashSet<String>>>,
    pub ready_cache: Arc<RwLock<ReadyCache>>,
    pub metrics: Arc<Metrics>,
    pub shutdown_token: CancellationToken,
    /// Per-order-key count of consecutive `freeze_order` failures.
    /// Once a key reaches MAX_CONSECUTIVE_FREEZE_FAILURES it is added to
    /// `frozen_order_blacklist` and never retried again (#498).
    pub freeze_failure_counts: Arc<Mutex<HashMap<String, u32>>>,
    /// Order keys that have been permanently abandoned after too many
    /// consecutive freeze failures (#498).
    pub frozen_order_blacklist: Arc<Mutex<HashMap<String, u32>>>,
}

impl AppState {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            http: crate::http::client().clone(),
            price_cache: Arc::new(RwLock::new(PriceCache::default())),
            cycle_status: Arc::new(RwLock::new(CycleStatus::default())),
            failures: Arc::new(Mutex::new(RingBuffer::default())),
            keeper_status: Arc::new(RwLock::new(KeeperStatus::default())),
            in_flight_keys: Arc::new(Mutex::new(std::collections::HashSet::new())),
            ready_cache: Arc::new(RwLock::new(ReadyCache::default())),
            metrics: Metrics::new(),
            shutdown_token: CancellationToken::new(),
            freeze_failure_counts: Arc::new(Mutex::new(HashMap::new())),
            frozen_order_blacklist: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        KeeperExecution, KeeperStatus, RingBuffer, FAILURE_RING_CAPACITY,
        KEEPER_EXECUTION_RING_CAPACITY,
    };
    use std::time::SystemTime;

    #[test]
    fn ring_buffer_evicts_oldest_items_at_capacity() {
        let mut buffer = RingBuffer::new(2);

        buffer.push(1);
        buffer.push(2);
        buffer.push(3);

        let items = buffer.iter().copied().collect::<Vec<_>>();
        assert_eq!(items, vec![2, 3]);
    }

    #[test]
    fn ring_buffer_with_zero_capacity_never_grows() {
        let mut buffer = RingBuffer::new(0);

        buffer.push(1);
        buffer.push(2);
        buffer.push(3);

        assert_eq!(buffer.iter().count(), 0);
    }

    #[test]
    fn keeper_status_keeps_only_the_most_recent_executions() {
        // The bound the manual `Vec::remove(0)` trim used to enforce, now enforced by the buffer.
        // Nothing covered it before, so a capacity change would have gone unnoticed.
        let mut status = KeeperStatus::default();
        for i in 0..KEEPER_EXECUTION_RING_CAPACITY + 25 {
            status.last_executions.push(KeeperExecution {
                timestamp: SystemTime::now(),
                operation: format!("op-{i}"),
                key: String::new(),
                tx_hash: None,
                success: true,
                error: None,
            });
        }

        assert_eq!(
            status.last_executions.iter().count(),
            KEEPER_EXECUTION_RING_CAPACITY
        );
        // Oldest evicted, newest retained, order preserved.
        assert_eq!(
            status.last_executions.iter().next().unwrap().operation,
            format!("op-{}", 25)
        );
        assert_eq!(
            status.last_executions.iter().next_back().unwrap().operation,
            format!("op-{}", KEEPER_EXECUTION_RING_CAPACITY + 24)
        );
    }

    #[test]
    fn keeper_status_default_uses_its_own_capacity_not_the_failure_ring() {
        // RingBuffer::default() is FAILURE_RING_CAPACITY (256). If KeeperStatus ever goes back to a
        // derived Default, this list silently grows to 256 and this test is what says so.
        let mut status = KeeperStatus::default();
        for i in 0..FAILURE_RING_CAPACITY {
            status.last_executions.push(KeeperExecution {
                timestamp: SystemTime::now(),
                operation: format!("op-{i}"),
                key: String::new(),
                tx_hash: None,
                success: true,
                error: None,
            });
        }
        assert_eq!(
            status.last_executions.iter().count(),
            KEEPER_EXECUTION_RING_CAPACITY
        );
    }
}
