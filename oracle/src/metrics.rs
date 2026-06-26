use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

#[derive(Debug, Default)]
pub struct Metrics {
    pub price_cycle_count: AtomicU64,
    pub price_cycle_latency_ms: AtomicU64,
    pub token_fetch_ok: AtomicU64,
    pub token_fetch_failures: AtomicU64,
    pub keeper_cycle_count: AtomicU64,
    pub keeper_cycle_latency_ms: AtomicU64,
    pub orders_executed: AtomicU64,
    pub deposits_executed: AtomicU64,
    pub withdrawals_executed: AtomicU64,
    pub submit_failures: AtomicU64,
    pub last_metrics_update: AtomicU64,
}

#[derive(Debug, Serialize)]
pub struct MetricsResponse {
    pub price_cycle_count: u64,
    pub price_cycle_latency_ms: u64,
    pub token_fetch_ok: u64,
    pub token_fetch_failures: u64,
    pub keeper_cycle_count: u64,
    pub keeper_cycle_latency_ms: u64,
    pub orders_executed: u64,
    pub deposits_executed: u64,
    pub withdrawals_executed: u64,
    pub submit_failures: u64,
    pub last_metrics_update: u64,
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn record_price_cycle(&self, latency_ms: u64, tokens_ok: usize, tokens_failed: usize) {
        self.price_cycle_count.fetch_add(1, Ordering::Relaxed);
        self.price_cycle_latency_ms
            .store(latency_ms, Ordering::Relaxed);
        self.token_fetch_ok
            .fetch_add(tokens_ok as u64, Ordering::Relaxed);
        self.token_fetch_failures
            .fetch_add(tokens_failed as u64, Ordering::Relaxed);
        self.update_timestamp();
    }

    pub fn record_keeper_cycle(
        &self,
        latency_ms: u64,
        orders: usize,
        deposits: usize,
        withdrawals: usize,
        errors: usize,
    ) {
        self.keeper_cycle_count.fetch_add(1, Ordering::Relaxed);
        self.keeper_cycle_latency_ms
            .store(latency_ms, Ordering::Relaxed);
        self.orders_executed
            .fetch_add(orders as u64, Ordering::Relaxed);
        self.deposits_executed
            .fetch_add(deposits as u64, Ordering::Relaxed);
        self.withdrawals_executed
            .fetch_add(withdrawals as u64, Ordering::Relaxed);
        self.submit_failures
            .fetch_add(errors as u64, Ordering::Relaxed);
        self.update_timestamp();
    }

    pub fn record_submit_failure(&self) {
        self.submit_failures.fetch_add(1, Ordering::Relaxed);
        self.update_timestamp();
    }

    fn update_timestamp(&self) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.last_metrics_update.store(timestamp, Ordering::Relaxed);
    }

    pub fn to_response(&self) -> MetricsResponse {
        MetricsResponse {
            price_cycle_count: self.price_cycle_count.load(Ordering::Relaxed),
            price_cycle_latency_ms: self.price_cycle_latency_ms.load(Ordering::Relaxed),
            token_fetch_ok: self.token_fetch_ok.load(Ordering::Relaxed),
            token_fetch_failures: self.token_fetch_failures.load(Ordering::Relaxed),
            keeper_cycle_count: self.keeper_cycle_count.load(Ordering::Relaxed),
            keeper_cycle_latency_ms: self.keeper_cycle_latency_ms.load(Ordering::Relaxed),
            orders_executed: self.orders_executed.load(Ordering::Relaxed),
            deposits_executed: self.deposits_executed.load(Ordering::Relaxed),
            withdrawals_executed: self.withdrawals_executed.load(Ordering::Relaxed),
            submit_failures: self.submit_failures.load(Ordering::Relaxed),
            last_metrics_update: self.last_metrics_update.load(Ordering::Relaxed),
        }
    }

    pub fn to_prometheus(&self) -> String {
        let mut output = String::new();

        output.push_str("# HELP oracle_price_cycle_count Total number of price cycles\n");
        output.push_str("# TYPE oracle_price_cycle_count counter\n");
        output.push_str(&format!(
            "oracle_price_cycle_count {}\n",
            self.price_cycle_count.load(Ordering::Relaxed)
        ));

        output.push_str(
            "# HELP oracle_price_cycle_latency_ms Last price cycle latency in milliseconds\n",
        );
        output.push_str("# TYPE oracle_price_cycle_latency_ms gauge\n");
        output.push_str(&format!(
            "oracle_price_cycle_latency_ms {}\n",
            self.price_cycle_latency_ms.load(Ordering::Relaxed)
        ));

        output.push_str("# HELP oracle_keeper_cycle_count Total number of keeper cycles\n");
        output.push_str("# TYPE oracle_keeper_cycle_count counter\n");
        output.push_str(&format!(
            "oracle_keeper_cycle_count {}\n",
            self.keeper_cycle_count.load(Ordering::Relaxed)
        ));

        output.push_str(
            "# HELP oracle_keeper_cycle_latency_ms Last keeper cycle latency in milliseconds\n",
        );
        output.push_str("# TYPE oracle_keeper_cycle_latency_ms gauge\n");
        output.push_str(&format!(
            "oracle_keeper_cycle_latency_ms {}\n",
            self.keeper_cycle_latency_ms.load(Ordering::Relaxed)
        ));

        output.push_str("# HELP oracle_orders_executed Total number of orders executed\n");
        output.push_str("# TYPE oracle_orders_executed counter\n");
        output.push_str(&format!(
            "oracle_orders_executed {}\n",
            self.orders_executed.load(Ordering::Relaxed)
        ));

        output.push_str("# HELP oracle_deposits_executed Total number of deposits executed\n");
        output.push_str("# TYPE oracle_deposits_executed counter\n");
        output.push_str(&format!(
            "oracle_deposits_executed {}\n",
            self.deposits_executed.load(Ordering::Relaxed)
        ));

        output
            .push_str("# HELP oracle_withdrawals_executed Total number of withdrawals executed\n");
        output.push_str("# TYPE oracle_withdrawals_executed counter\n");
        output.push_str(&format!(
            "oracle_withdrawals_executed {}\n",
            self.withdrawals_executed.load(Ordering::Relaxed)
        ));

        output.push_str("# HELP oracle_token_fetch_ok_total Total individual token fetch successes across all price cycles\n");
        output.push_str("# TYPE oracle_token_fetch_ok_total counter\n");
        output.push_str(&format!(
            "oracle_token_fetch_ok_total {}\n",
            self.token_fetch_ok.load(Ordering::Relaxed)
        ));

        output.push_str("# HELP oracle_token_fetch_failures_total Total individual token fetch failures across all price cycles\n");
        output.push_str("# TYPE oracle_token_fetch_failures_total counter\n");
        output.push_str(&format!(
            "oracle_token_fetch_failures_total {}\n",
            self.token_fetch_failures.load(Ordering::Relaxed)
        ));

        output.push_str("# HELP oracle_submit_failures Total number of submit failures\n");
        output.push_str("# TYPE oracle_submit_failures counter\n");
        output.push_str(&format!(
            "oracle_submit_failures {}\n",
            self.submit_failures.load(Ordering::Relaxed)
        ));

        output.push_str("# HELP oracle_last_metrics_update Timestamp of last metrics update\n");
        output.push_str("# TYPE oracle_last_metrics_update gauge\n");
        output.push_str(&format!(
            "oracle_last_metrics_update {}\n",
            self.last_metrics_update.load(Ordering::Relaxed)
        ));

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_recording() {
        let metrics = Metrics::new();
        metrics.record_price_cycle(100, 3, 1);
        metrics.record_keeper_cycle(200, 5, 3, 2, 1);

        let response = metrics.to_response();
        assert_eq!(response.price_cycle_count, 1);
        assert_eq!(response.price_cycle_latency_ms, 100);
        assert_eq!(response.keeper_cycle_count, 1);
        assert_eq!(response.keeper_cycle_latency_ms, 200);
        assert_eq!(response.orders_executed, 5);
        assert_eq!(response.deposits_executed, 3);
        assert_eq!(response.withdrawals_executed, 2);
        assert_eq!(response.submit_failures, 1);
    }

    #[test]
    fn test_prometheus_output() {
        let metrics = Metrics::new();
        metrics.record_price_cycle(100, 3, 1);

        let prometheus = metrics.to_prometheus();
        assert!(prometheus.contains("oracle_price_cycle_count 1"));
        assert!(prometheus.contains("oracle_price_cycle_latency_ms 100"));
    }

    #[test]
    fn token_fetch_failures_accumulates_across_price_cycles() {
        let metrics = Metrics::new();
        metrics.record_price_cycle(50, 2, 1);
        metrics.record_price_cycle(60, 3, 2);

        let resp = metrics.to_response();
        assert_eq!(resp.token_fetch_failures, 3, "1 + 2 = 3 total failures");
    }

    #[test]
    fn token_fetch_ok_accumulates_across_price_cycles() {
        let metrics = Metrics::new();
        metrics.record_price_cycle(50, 4, 0);
        metrics.record_price_cycle(60, 2, 1);

        let resp = metrics.to_response();
        assert_eq!(resp.token_fetch_ok, 6, "4 + 2 = 6 total successes");
    }
}
