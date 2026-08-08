use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// All price-cycle and keeper-cycle counters, held behind a single mutex.
///
/// These fields used to be independent `AtomicU64`s, each updated with its
/// own `Relaxed` store/fetch_add. A recording call touching several of them
/// (e.g. `record_price_cycle` bumping the cycle count, latency, and both
/// token-fetch totals) was therefore not atomic as a group: a concurrent
/// reader could observe some fields from the new cycle and some from the
/// old one. Grouping them behind one mutex makes every `record_*` call a
/// single critical section, so readers always see one consistent
/// generation (resolves #599).
#[derive(Debug, Default)]
struct Counters {
    price_cycle_count: u64,
    price_cycle_latency_ms: u64,
    token_fetch_ok: u64,
    token_fetch_failures: u64,
    keeper_cycle_count: u64,
    keeper_cycle_latency_ms: u64,
    orders_executed: u64,
    deposits_executed: u64,
    withdrawals_executed: u64,
    submit_failures: u64,
    last_metrics_update: u64,
}

#[derive(Debug, Default)]
pub struct Metrics {
    counters: Mutex<Counters>,
    pub token_source_fetch_failures: Mutex<BTreeMap<TokenSourceLabels, u64>>,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct TokenSourceLabels {
    pub symbol: String,
    pub token: String,
    pub source: String,
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
        let mut c = self.counters.lock().unwrap_or_else(|p| p.into_inner());
        c.price_cycle_count += 1;
        c.price_cycle_latency_ms = latency_ms;
        c.token_fetch_ok += tokens_ok as u64;
        c.token_fetch_failures += tokens_failed as u64;
        Self::stamp(&mut c);
    }

    pub fn record_keeper_cycle(
        &self,
        latency_ms: u64,
        orders: usize,
        deposits: usize,
        withdrawals: usize,
        errors: usize,
    ) {
        let mut c = self.counters.lock().unwrap_or_else(|p| p.into_inner());
        c.keeper_cycle_count += 1;
        c.keeper_cycle_latency_ms = latency_ms;
        c.orders_executed += orders as u64;
        c.deposits_executed += deposits as u64;
        c.withdrawals_executed += withdrawals as u64;
        c.submit_failures += errors as u64;
        Self::stamp(&mut c);
    }

    pub fn record_submit_failure(&self) {
        let mut c = self.counters.lock().unwrap_or_else(|p| p.into_inner());
        c.submit_failures += 1;
        Self::stamp(&mut c);
    }

    pub fn record_token_source_fetch_failure(&self, symbol: &str, token: &str, source: &str) {
        let labels = TokenSourceLabels {
            symbol: symbol.to_string(),
            token: token.to_string(),
            source: source.to_string(),
        };
        let mut failures = self
            .token_source_fetch_failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *failures.entry(labels).or_insert(0) += 1;
        drop(failures);
        let mut c = self.counters.lock().unwrap_or_else(|p| p.into_inner());
        Self::stamp(&mut c);
    }

    /// Stamp the last-update time as part of the same locked critical
    /// section as the counters it accompanies, instead of a separate
    /// unsynchronized atomic store.
    fn stamp(c: &mut Counters) {
        c.last_metrics_update = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
    }

    pub fn to_response(&self) -> MetricsResponse {
        let c = self.counters.lock().unwrap_or_else(|p| p.into_inner());
        MetricsResponse {
            price_cycle_count: c.price_cycle_count,
            price_cycle_latency_ms: c.price_cycle_latency_ms,
            token_fetch_ok: c.token_fetch_ok,
            token_fetch_failures: c.token_fetch_failures,
            keeper_cycle_count: c.keeper_cycle_count,
            keeper_cycle_latency_ms: c.keeper_cycle_latency_ms,
            orders_executed: c.orders_executed,
            deposits_executed: c.deposits_executed,
            withdrawals_executed: c.withdrawals_executed,
            submit_failures: c.submit_failures,
            last_metrics_update: c.last_metrics_update,
        }
    }

    pub fn to_prometheus(&self) -> String {
        let c = self.counters.lock().unwrap_or_else(|p| p.into_inner());
        let mut output = String::new();

        output.push_str("# HELP oracle_price_cycle_count Total number of price cycles\n");
        output.push_str("# TYPE oracle_price_cycle_count counter\n");
        output.push_str(&format!(
            "oracle_price_cycle_count {}\n",
            c.price_cycle_count
        ));

        output.push_str(
            "# HELP oracle_price_cycle_latency_ms Last price cycle latency in milliseconds\n",
        );
        output.push_str("# TYPE oracle_price_cycle_latency_ms gauge\n");
        output.push_str(&format!(
            "oracle_price_cycle_latency_ms {}\n",
            c.price_cycle_latency_ms
        ));

        output.push_str("# HELP oracle_keeper_cycle_count Total number of keeper cycles\n");
        output.push_str("# TYPE oracle_keeper_cycle_count counter\n");
        output.push_str(&format!(
            "oracle_keeper_cycle_count {}\n",
            c.keeper_cycle_count
        ));

        output.push_str(
            "# HELP oracle_keeper_cycle_latency_ms Last keeper cycle latency in milliseconds\n",
        );
        output.push_str("# TYPE oracle_keeper_cycle_latency_ms gauge\n");
        output.push_str(&format!(
            "oracle_keeper_cycle_latency_ms {}\n",
            c.keeper_cycle_latency_ms
        ));

        output.push_str("# HELP oracle_orders_executed Total number of orders executed\n");
        output.push_str("# TYPE oracle_orders_executed counter\n");
        output.push_str(&format!("oracle_orders_executed {}\n", c.orders_executed));

        output.push_str("# HELP oracle_deposits_executed Total number of deposits executed\n");
        output.push_str("# TYPE oracle_deposits_executed counter\n");
        output.push_str(&format!(
            "oracle_deposits_executed {}\n",
            c.deposits_executed
        ));

        output
            .push_str("# HELP oracle_withdrawals_executed Total number of withdrawals executed\n");
        output.push_str("# TYPE oracle_withdrawals_executed counter\n");
        output.push_str(&format!(
            "oracle_withdrawals_executed {}\n",
            c.withdrawals_executed
        ));

        output.push_str("# HELP oracle_token_fetch_ok_total Total individual token fetch successes across all price cycles\n");
        output.push_str("# TYPE oracle_token_fetch_ok_total counter\n");
        output.push_str(&format!(
            "oracle_token_fetch_ok_total {}\n",
            c.token_fetch_ok
        ));

        output.push_str("# HELP oracle_token_fetch_failures_total Total individual token fetch failures across all price cycles\n");
        output.push_str("# TYPE oracle_token_fetch_failures_total counter\n");
        output.push_str(&format!(
            "oracle_token_fetch_failures_total {}\n",
            c.token_fetch_failures
        ));

        output.push_str("# HELP oracle_token_source_fetch_failures_total Total source fetch failures by configured token and source\n");
        output.push_str("# TYPE oracle_token_source_fetch_failures_total counter\n");
        for (labels, count) in self
            .token_source_fetch_failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
        {
            output.push_str(&format!(
                "oracle_token_source_fetch_failures_total{{symbol=\"{}\",token=\"{}\",source=\"{}\"}} {}\n",
                escape_label_value(&labels.symbol),
                escape_label_value(&labels.token),
                escape_label_value(&labels.source),
                count
            ));
        }

        output.push_str("# HELP oracle_submit_failures Total number of submit failures\n");
        output.push_str("# TYPE oracle_submit_failures counter\n");
        output.push_str(&format!("oracle_submit_failures {}\n", c.submit_failures));

        output.push_str("# HELP oracle_last_metrics_update Timestamp of last metrics update\n");
        output.push_str("# TYPE oracle_last_metrics_update gauge\n");
        output.push_str(&format!(
            "oracle_last_metrics_update {}\n",
            c.last_metrics_update
        ));

        output
    }
}

fn escape_label_value(value: &str) -> String {
    value
        .replace('\\', r"\\")
        .replace('\n', r"\n")
        .replace('"', r#"\""#)
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

    #[test]
    fn token_source_failures_are_exported_with_bounded_labels() {
        let metrics = Metrics::new();
        metrics.record_token_source_fetch_failure("XLM", "CXLM", "pyth");
        metrics.record_token_source_fetch_failure("XLM", "CXLM", "pyth");
        metrics.record_token_source_fetch_failure("BTC", "CBTC", "binance");

        let prometheus = metrics.to_prometheus();
        assert!(prometheus.contains(
            "oracle_token_source_fetch_failures_total{symbol=\"XLM\",token=\"CXLM\",source=\"pyth\"} 2"
        ));
        assert!(prometheus.contains(
            "oracle_token_source_fetch_failures_total{symbol=\"BTC\",token=\"CBTC\",source=\"binance\"} 1"
        ));
    }

    #[test]
    fn prometheus_label_values_are_escaped() {
        let metrics = Metrics::new();
        metrics.record_token_source_fetch_failure("A\"B", "C\\D", "line\nfeed");

        let prometheus = metrics.to_prometheus();
        assert!(prometheus.contains(
            "oracle_token_source_fetch_failures_total{symbol=\"A\\\"B\",token=\"C\\\\D\",source=\"line\\nfeed\"} 1"
        ));
    }
}
