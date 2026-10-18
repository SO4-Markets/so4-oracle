use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_submitted_medians: Option<BTreeMap<String, i128>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_full_submission: Option<SystemTime>,
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

#[derive(Debug, Default, Clone, Serialize)]
pub struct KeeperStatus {
    pub pending_orders: usize,
    pub pending_deposits: usize,
    pub pending_withdrawals: usize,
    pub last_executions: VecDeque<KeeperExecution>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeeperExecution {
    pub timestamp: SystemTime,
    pub operation: String,
    pub key: String,
    pub tx_hash: Option<String>,
    /// Ledger at which the transaction was confirmed (#1289). `None` for
    /// failures, and for poll timeouts whose outcome is still undecided.
    pub ledger: Option<u32>,
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

#[derive(Debug, Default, Clone)]
pub struct ReadyCache {
    pub last_checked: Option<std::time::Instant>,
    pub last_error: Option<(axum::http::StatusCode, String)>,
}

/// Number of consecutive `freeze_order` failures before a key is permanently
/// skipped and a loud alert is emitted (#498).
pub const MAX_CONSECUTIVE_FREEZE_FAILURES: u32 = 3;

/// Number of consecutive `execute_order` failures (of *any* kind — not just
/// the budget-exceeded case #498 handled) before a key is abandoned: the
/// keeper stops re-submitting a permanently-broken order that would otherwise
/// re-fail and burn a transaction fee on every cycle forever (#803).
pub const MAX_CONSECUTIVE_EXECUTION_FAILURES: u32 = 5;

/// Maximum time a key can remain "in flight" after a poll timeout before it
/// is automatically evicted with a warning (#801). 5 minutes is generous
/// enough for Stellar confirmations but short enough to unblock silently
/// stuck orders within a handful of keeper cycles.
pub const IN_FLIGHT_EXPIRY: Duration = Duration::from_secs(300);

/// Failed admin-auth attempts allowed per client within [`ADMIN_AUTH_WINDOW`]
/// before further attempts are rejected with 429 until the window ends (#1214).
pub const MAX_ADMIN_AUTH_FAILURES: u32 = 5;
pub const ADMIN_AUTH_WINDOW: Duration = Duration::from_secs(60);
/// Cap on tracked clients so the map can't grow without bound.
const MAX_TRACKED_ADMIN_CLIENTS: usize = 10_000;

/// Per-client failure counter for `AdminAuth` (#1214). Keyed by client IP
/// (or "unknown" when the peer address isn't available, e.g. in tests).
#[derive(Default)]
pub struct AdminAuthLimiter {
    failures: std::sync::Mutex<HashMap<String, (u32, Instant)>>,
}

impl AdminAuthLimiter {
    /// True when `client` has used up its failure budget for the current window.
    pub fn is_blocked(&self, client: &str, now: Instant) -> bool {
        let map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        matches!(map.get(client), Some((count, start))
            if *count >= MAX_ADMIN_AUTH_FAILURES && now.duration_since(*start) < ADMIN_AUTH_WINDOW)
    }

    pub fn record_failure(&self, client: &str, now: Instant) {
        let mut map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= MAX_TRACKED_ADMIN_CLIENTS {
            map.retain(|_, (_, start)| now.duration_since(*start) < ADMIN_AUTH_WINDOW);
        }
        let entry = map.entry(client.to_string()).or_insert((0, now));
        if now.duration_since(entry.1) >= ADMIN_AUTH_WINDOW {
            *entry = (0, now);
        }
        entry.0 += 1;
    }

    pub fn record_success(&self, client: &str) {
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(client);
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub http: reqwest::Client,
    pub price_cache: Arc<RwLock<PriceCache>>,
    pub cycle_status: Arc<RwLock<CycleStatus>>,
    pub failures: Arc<Mutex<RingBuffer<FailedSubmission>>>,
    pub keeper_status: Arc<RwLock<KeeperStatus>>,
    pub in_flight_keys: Arc<Mutex<HashMap<String, Instant>>>,
    pub ready_cache: Arc<RwLock<ReadyCache>>,
    pub metrics: Arc<Metrics>,
    pub shutdown_token: CancellationToken,
    /// Per-order-key count of consecutive `freeze_order` failures.
    /// Once a key reaches MAX_CONSECUTIVE_FREEZE_FAILURES it is added to
    /// `frozen_order_blacklist` and never retried again (#498).
    pub freeze_failure_counts: Arc<Mutex<HashMap<String, u32>>>,
    /// Order keys that have been permanently abandoned after too many
    /// consecutive freeze failures (#498) or execute failures of any other
    /// kind (#803).
    pub frozen_order_blacklist: Arc<Mutex<HashMap<String, u32>>>,
    /// Per-order-key count of consecutive `execute_order` failures of any
    /// kind. Reset on the first success. Once a key reaches
    /// MAX_CONSECUTIVE_EXECUTION_FAILURES it is blacklisted (#803).
    pub execution_failure_counts: Arc<Mutex<HashMap<String, u32>>>,
    /// Tracks whether the keeper balance is currently below the minimum.
    /// Scoped to AppState instead of a bare process-global to avoid races
    /// between concurrent /ready and /keeper/balance checks (#737).
    pub keeper_balance_below_min: Arc<AtomicBool>,
    /// Bumped once at the start and once at the end of every `run_keeper_cycle`.
    /// `keeper_status_snapshot` uses it to detect a keeper-cycle boundary that
    /// crossed its two sequential lock reads and retry, rather than returning a
    /// torn `keeper_status` / `cycle_status` pair (#797).
    pub keeper_cycle_generation: Arc<AtomicU64>,
    /// `KEEPER_PRIVATE_KEY` decoded once at startup for price signing (#1226).
    /// `SigningKey` zeroises its seed on drop. `None` if unset or invalid, in
    /// which case signing falls back to the string key and reports the error.
    pub price_signing_key: Option<Arc<ed25519_dalek::SigningKey>>,
    /// `KEEPER_SECRET_KEY` decoded once at startup for transaction signing (#1226).
    pub tx_signing_key: Option<Arc<ed25519_dalek::SigningKey>>,
    /// Throttles repeated failed admin-token attempts per client (#1214).
    pub admin_auth_limiter: Arc<AdminAuthLimiter>,
}

impl AppState {
    pub fn new(config: Arc<Config>) -> Self {
        let price_signing_key =
            crate::signing::signing_key_from_hex(config.keeper_private_key.as_str())
                .ok()
                .map(Arc::new);
        let tx_signing_key =
            crate::chain::tx_builder::signing_key_from_secret(config.keeper_secret_key.as_str())
                .ok()
                .map(Arc::new);
        Self {
            config,
            http: crate::http::client().clone(),
            price_cache: Arc::new(RwLock::new(PriceCache::default())),
            cycle_status: Arc::new(RwLock::new(CycleStatus::default())),
            failures: Arc::new(Mutex::new(RingBuffer::default())),
            keeper_status: Arc::new(RwLock::new(KeeperStatus::default())),
            in_flight_keys: Arc::new(Mutex::new(HashMap::new())),
            ready_cache: Arc::new(RwLock::new(ReadyCache::default())),
            metrics: Metrics::new(),
            shutdown_token: CancellationToken::new(),
            freeze_failure_counts: Arc::new(Mutex::new(HashMap::new())),
            frozen_order_blacklist: Arc::new(Mutex::new(HashMap::new())),
            execution_failure_counts: Arc::new(Mutex::new(HashMap::new())),
            keeper_balance_below_min: Arc::new(AtomicBool::new(false)),
            keeper_cycle_generation: Arc::new(AtomicU64::new(0)),
            price_signing_key,
            tx_signing_key,
            admin_auth_limiter: Arc::new(AdminAuthLimiter::default()),
        }
    }

    /// Reads `keeper_status` and `cycle_status` as one consistent snapshot for
    /// `GET /keeper/status`, which needs `pending_*` / `last_executions` from
    /// the former and `last_keeper_cycle_*` from the latter.
    ///
    /// Acquiring the two `RwLock`s in sequence can straddle a `run_keeper_cycle`
    /// and return pending counts from before the cycle paired with timing from
    /// after it (#797). `run_keeper_cycle` bumps `keeper_cycle_generation` at
    /// its start and end, and within a cycle it always writes `keeper_status`
    /// (pending counts) strictly before the `cycle_status` keeper-timing fields;
    /// so if the generation is unchanged across both reads, no cycle boundary
    /// crossed them and the pair is from a single point in time. Otherwise we
    /// retry.
    pub async fn keeper_status_snapshot(&self) -> (KeeperStatus, CycleStatus) {
        use std::sync::atomic::Ordering;

        for _ in 0..8 {
            let generation = self.keeper_cycle_generation.load(Ordering::Acquire);
            let keeper_status = self.keeper_status.read().await.clone();
            let cycle_status = self.cycle_status.read().await.clone();
            if self.keeper_cycle_generation.load(Ordering::Acquire) == generation {
                return (keeper_status, cycle_status);
            }
        }

        // A keeper cycle completes at most once per keeper_loop_interval while
        // these two reads take microseconds, so 8 consecutive collisions is not
        // realistically reachable; fall back to a best-effort pair.
        (
            self.keeper_status.read().await.clone(),
            self.cycle_status.read().await.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::RingBuffer;

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
}

#[cfg(test)]
mod admin_auth_limiter_tests {
    use super::*;

    #[test]
    fn blocks_after_max_failures_and_resets_after_window() {
        let limiter = AdminAuthLimiter::default();
        let t0 = Instant::now();
        for _ in 0..MAX_ADMIN_AUTH_FAILURES {
            assert!(!limiter.is_blocked("1.2.3.4", t0));
            limiter.record_failure("1.2.3.4", t0);
        }
        assert!(limiter.is_blocked("1.2.3.4", t0));
        assert!(
            !limiter.is_blocked("5.6.7.8", t0),
            "other clients are unaffected"
        );
        assert!(!limiter.is_blocked("1.2.3.4", t0 + ADMIN_AUTH_WINDOW));
    }

    #[test]
    fn success_clears_failures() {
        let limiter = AdminAuthLimiter::default();
        let t0 = Instant::now();
        for _ in 0..MAX_ADMIN_AUTH_FAILURES - 1 {
            limiter.record_failure("1.2.3.4", t0);
        }
        limiter.record_success("1.2.3.4");
        limiter.record_failure("1.2.3.4", t0);
        assert!(!limiter.is_blocked("1.2.3.4", t0));
    }
}

#[cfg(test)]
mod signing_key_state_tests {
    use super::*;

    // #1226 — keys are decoded once when AppState is built, not per signature.
    #[test]
    fn app_state_decodes_valid_keys_once_and_skips_invalid_ones() {
        let mut config = Config::default_for_tests();
        config.keeper_private_key = crate::config::SecretString::new("11".repeat(32));
        config.keeper_secret_key = crate::config::SecretString::new("22".repeat(32));
        let state = AppState::new(Arc::new(config));
        assert!(state.price_signing_key.is_some());
        assert!(state.tx_signing_key.is_some());

        let state = AppState::new(Arc::new(Config::default_for_tests()));
        assert!(
            state.price_signing_key.is_none(),
            "empty key must not decode"
        );
        assert!(state.tx_signing_key.is_none(), "empty key must not decode");
    }
}
