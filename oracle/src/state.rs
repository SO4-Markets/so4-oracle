use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
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
    #[serde(skip_serializing)]
    pub sources_used: Vec<String>,
    pub signature: String,
}

impl CachedPrice {
    pub fn age_seconds(&self, now: u64) -> u64 {
        now.saturating_sub(self.timestamp)
    }

    pub fn is_stale(&self, stale_after_seconds: u64, now: u64) -> bool {
        match now.checked_sub(self.timestamp) {
            Some(age) => age >= stale_after_seconds,
            None => true,
        }
    }
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
    /// Price source that failed (`binance`, `coinbase`, ...), for per-source
    /// fetch failures; `None` for failures not tied to one source (#1450).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
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
    /// `KEEPER_PRIVATE_KEY` decoded once at config load (#1226, #1473).
    /// `SigningKey` zeroises its seed on drop. `None` when unset; an
    /// undecodable key is a boot failure, not a per-signature fallback.
    /// Last startup preflight result, exposed on `/oracle/status` (#1468).
    pub preflight_result: Arc<RwLock<Option<crate::preflight::PreflightResult>>>,
    /// `KEEPER_PRIVATE_KEY` decoded once at startup for price signing (#1226).
    /// `SigningKey` zeroises its seed on drop. `None` if unset or invalid, in
    /// which case signing falls back to the string key and reports the error.
    pub price_signing_key: Option<Arc<ed25519_dalek::SigningKey>>,
    /// `KEEPER_SECRET_KEY` decoded once at config load (#1226, #1473).
    /// `None` when unset; an undecodable key is a boot failure.
    pub tx_signing_key: Option<Arc<ed25519_dalek::SigningKey>>,
    /// Throttles repeated failed admin-token attempts per client (#1214).
    pub admin_auth_limiter: Arc<AdminAuthLimiter>,
    /// Round-robin start index for `get_pending_keys` fetches so keys near
    /// the end of the on-chain list are not permanently starved when the list
    /// is longer than the batch each cycle can process (#1268).
    pub pending_key_start_index: Arc<AtomicU32>,
}

impl AppState {
    pub fn new(config: Arc<Config>) -> Self {
        // Keys were decoded (and the raw strings dropped) at config load
        // (#1472, #1473); AppState just carries the Arc'd decoded keys.
        let price_signing_key = config.keeper_private_key.clone();
        let tx_signing_key = config.keeper_secret_key.clone();
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
            preflight_result: Arc::new(RwLock::new(None)),
            price_signing_key,
            tx_signing_key,
            admin_auth_limiter: Arc::new(AdminAuthLimiter::default()),
            pending_key_start_index: Arc::new(AtomicU32::new(0)),
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

    // #1226/#1473 — keys are decoded at config load; AppState carries them.
    #[test]
    fn app_state_carries_decoded_keys_from_config() {
        let mut config = Config::default_for_tests();
        config.keeper_private_key = Some(std::sync::Arc::new(
            crate::signing::signing_key_from_hex(&"11".repeat(32)).unwrap(),
        ));
        config.keeper_secret_key = Some(std::sync::Arc::new(
            crate::chain::tx_builder::signing_key_from_secret(
                "SAUHMCMUP5FZO5675W3ISZ6E6CNYJGXBUW5WANE2JR4TGAARYCTSCBKI",
            )
            .unwrap(),
        ));
        let state = AppState::new(Arc::new(config));
        assert!(state.price_signing_key.is_some());
        assert!(state.tx_signing_key.is_some());

        let state = AppState::new(Arc::new(Config::default_for_tests()));
        assert!(
            state.price_signing_key.is_none(),
            "unset key must stay None"
        );
        assert!(state.tx_signing_key.is_none(), "unset key must stay None");
    }
}

#[cfg(test)]
mod keeper_status_snapshot_tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    // #1408 (1): with a stable generation the pair is returned from the first read.
    #[tokio::test]
    async fn snapshot_returns_pair_when_generation_unchanged() {
        let state = AppState::new(Arc::new(Config::default_for_tests()));
        state.keeper_status.write().await.pending_orders = 3;
        state.cycle_status.write().await.last_keeper_cycle_latency_ms = Some(7);

        let (ks, cs) = state.keeper_status_snapshot().await;
        assert_eq!(ks.pending_orders, 3);
        assert_eq!(cs.last_keeper_cycle_latency_ms, Some(7));
    }

    // #1408 (2): a generation bump between the two reads forces a retry, so
    // the returned keeper_status is re-read after the bump.
    #[tokio::test]
    async fn snapshot_retries_when_generation_bumps_between_reads() {
        let state = Arc::new(AppState::new(Arc::new(Config::default_for_tests())));
        state.keeper_status.write().await.pending_orders = 1;

        // Hold the second lock so the snapshot stalls between its two reads.
        let stall = state.cycle_status.write().await;
        let reader = tokio::spawn({
            let state = Arc::clone(&state);
            async move { state.keeper_status_snapshot().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        state.keeper_cycle_generation.fetch_add(1, Ordering::SeqCst);
        state.keeper_status.write().await.pending_orders = 2;
        drop(stall);

        let (ks, _) = reader.await.unwrap();
        assert_eq!(
            ks.pending_orders, 2,
            "snapshot must re-read after a generation bump"
        );
    }

    // #1408 (3): under permanent generation flux every attempt collides and
    // the call falls through to the best-effort pair instead of looping.
    #[tokio::test]
    async fn snapshot_falls_back_when_generation_never_settles() {
        let state = Arc::new(AppState::new(Arc::new(Config::default_for_tests())));
        state.keeper_status.write().await.pending_orders = 9;

        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flux = {
            let gen = Arc::clone(&state.keeper_cycle_generation);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    gen.fetch_add(1, Ordering::SeqCst);
                }
            })
        };

        let (ks, _) = state.keeper_status_snapshot().await;
        stop.store(true, Ordering::SeqCst);
        flux.join().unwrap();
        assert_eq!(ks.pending_orders, 9, "best-effort pair must still return");
    }
}
