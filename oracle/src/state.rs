//! Shared application state — held behind `Arc<RwLock<AppState>>`.

use crate::config::Config;
use crate::CachedPrice;

/// Root shared state for the service.
pub struct AppState {
    pub config: Config,
    pub http_client: reqwest::Client,
    pub price_cache: PriceCache,
    pub cycle_status: CycleStatus,
    pub failed_submissions: FailedSubmissionRing,
}

impl AppState {
    pub fn new(config: Config, http_client: reqwest::Client) -> Self {
        Self {
            config,
            http_client,
            price_cache: PriceCache::new(),
            cycle_status: CycleStatus::new(),
            failed_submissions: FailedSubmissionRing::new(64),
        }
    }
}

/// In-memory cache of the latest signed prices.
///
/// Written by `price_loop`, served by `GET /prices`.
#[derive(Debug, Clone)]
pub struct PriceCache {
    pub prices: Vec<CachedPrice>,
    pub updated_at: Option<u64>,
}

impl PriceCache {
    pub fn new() -> Self {
        Self {
            prices: Vec::new(),
            updated_at: None,
        }
    }
}

/// Tracks the most recent price-collection cycle.
#[derive(Debug, Clone, Default)]
pub struct CycleStatus {
    pub last_cycle_timestamp: Option<u64>,
    pub last_cycle_prices: usize,
    pub last_cycle_errors: Vec<String>,
    pub total_cycles: u64,
    pub total_errors: u64,
}

impl CycleStatus {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Bounded ring buffer of failed submission records.
#[derive(Debug, Clone)]
pub struct FailedSubmissionRing {
    capacity: usize,
    entries: Vec<FailedSubmission>,
    cursor: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FailedSubmission {
    pub token: String,
    pub error: String,
    pub timestamp: u64,
    pub ledger_seq: Option<u32>,
}

impl FailedSubmissionRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Vec::with_capacity(capacity),
            cursor: 0,
        }
    }

    pub fn push(&mut self, entry: FailedSubmission) {
        if self.entries.len() < self.capacity {
            self.entries.push(entry);
        } else {
            self.entries[self.cursor] = entry;
        }
        self.cursor = (self.cursor + 1) % self.capacity;
    }

    pub fn entries(&self) -> &[FailedSubmission] {
        &self.entries
    }
}
