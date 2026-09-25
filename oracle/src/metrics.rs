use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct HttpRouteLabels {
    pub route: String,
    pub method: String,
    pub status_class: String,
}

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
    http_requests_in_flight: u64,
    http_requests_total: BTreeMap<HttpRouteLabels, u64>,
    http_request_duration_buckets: BTreeMap<String, [u64; 7]>,
    http_request_duration_sum: BTreeMap<String, f64>,
    http_auth_failures_total: BTreeMap<String, u64>,
}

#[derive(Debug, Default)]
pub struct Metrics {
    counters: Mutex<Counters>,
    pub token_source_fetch_failures: Mutex<BTreeMap<TokenSourceLabels, u64>>,
    /// Per (symbol, token, source) count of times that source's price was
    /// excluded by the MAD-based outlier filter in `aggregate_prices`, even
    /// on cycles where enough other sources still met `min_sources` and the
    /// cycle succeeded. Surfaces `AggregatedPrice.rejected_sources`, which
    /// was previously computed on every successful aggregation and then
    /// discarded before reaching any consumer (#728).
    pub token_source_outlier_rejections: Mutex<BTreeMap<TokenSourceLabels, u64>>,
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

    /// Record that `source`'s price for `symbol`/`token` was excluded by the
    /// outlier filter during a price cycle (#728).
    pub fn record_token_source_outlier_rejection(&self, symbol: &str, token: &str, source: &str) {
        let labels = TokenSourceLabels {
            symbol: symbol.to_string(),
            token: token.to_string(),
            source: source.to_string(),
        };
        let mut rejections = self
            .token_source_outlier_rejections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *rejections.entry(labels).or_insert(0) += 1;
        drop(rejections);
        let mut c = self.counters.lock().unwrap_or_else(|p| p.into_inner());
        Self::stamp(&mut c);
    }

    pub fn inc_http_in_flight(&self) {
        let mut c = self.counters.lock().unwrap_or_else(|p| p.into_inner());
        c.http_requests_in_flight += 1;
    }

    pub fn dec_http_in_flight(&self) {
        let mut c = self.counters.lock().unwrap_or_else(|p| p.into_inner());
        c.http_requests_in_flight = c.http_requests_in_flight.saturating_sub(1);
    }

    pub fn record_http_request(&self, route: &str, method: &str, status: u16, latency_ms: u64) {
        let status_class = format!("{}xx", status / 100);
        let labels = HttpRouteLabels {
            route: route.to_string(),
            method: method.to_string(),
            status_class,
        };

        let mut c = self.counters.lock().unwrap_or_else(|p| p.into_inner());
        *c.http_requests_total.entry(labels).or_insert(0) += 1;

        let buckets = c
            .http_request_duration_buckets
            .entry(route.to_string())
            .or_insert([0; 7]);
        let latency_sec = latency_ms as f64 / 1000.0;
        if latency_sec <= 0.01 {
            buckets[0] += 1;
        }
        if latency_sec <= 0.05 {
            buckets[1] += 1;
        }
        if latency_sec <= 0.10 {
            buckets[2] += 1;
        }
        if latency_sec <= 0.25 {
            buckets[3] += 1;
        }
        if latency_sec <= 0.50 {
            buckets[4] += 1;
        }
        if latency_sec <= 1.00 {
            buckets[5] += 1;
        }
        buckets[6] += 1; // +Inf

        let sum = c
            .http_request_duration_sum
            .entry(route.to_string())
            .or_insert(0.0);
        *sum += latency_sec;
    }

    pub fn record_http_auth_failure(&self, route: &str) {
        let mut c = self.counters.lock().unwrap_or_else(|p| p.into_inner());
        *c.http_auth_failures_total
            .entry(route.to_string())
            .or_insert(0) += 1;
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

        output.push_str("# HELP oracle_price_cycles_total Total number of price cycles\n");
        output.push_str("# TYPE oracle_price_cycles_total counter\n");
        output.push_str(&format!(
            "oracle_price_cycles_total {}\n",
            c.price_cycle_count
        ));

        output.push_str(
            "# HELP oracle_price_cycle_duration_seconds Last price cycle duration in seconds\n",
        );
        output.push_str("# TYPE oracle_price_cycle_duration_seconds gauge\n");
        output.push_str(&format!(
            "oracle_price_cycle_duration_seconds {}\n",
            c.price_cycle_latency_ms as f64 / 1000.0
        ));

        output.push_str("# HELP oracle_keeper_cycles_total Total number of keeper cycles\n");
        output.push_str("# TYPE oracle_keeper_cycles_total counter\n");
        output.push_str(&format!(
            "oracle_keeper_cycles_total {}\n",
            c.keeper_cycle_count
        ));

        output.push_str(
            "# HELP oracle_keeper_cycle_duration_seconds Last keeper cycle duration in seconds\n",
        );
        output.push_str("# TYPE oracle_keeper_cycle_duration_seconds gauge\n");
        output.push_str(&format!(
            "oracle_keeper_cycle_duration_seconds {}\n",
            c.keeper_cycle_latency_ms as f64 / 1000.0
        ));

        output.push_str("# HELP oracle_orders_executed_total Total number of orders executed\n");
        output.push_str("# TYPE oracle_orders_executed_total counter\n");
        output.push_str(&format!(
            "oracle_orders_executed_total {}\n",
            c.orders_executed
        ));

        output
            .push_str("# HELP oracle_deposits_executed_total Total number of deposits executed\n");
        output.push_str("# TYPE oracle_deposits_executed_total counter\n");
        output.push_str(&format!(
            "oracle_deposits_executed_total {}\n",
            c.deposits_executed
        ));

        output.push_str(
            "# HELP oracle_withdrawals_executed_total Total number of withdrawals executed\n",
        );
        output.push_str("# TYPE oracle_withdrawals_executed_total counter\n");
        output.push_str(&format!(
            "oracle_withdrawals_executed_total {}\n",
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

        output.push_str("# HELP oracle_token_source_outlier_rejections_total Total times a source's price was excluded by the outlier filter, including cycles that otherwise succeeded\n");
        output.push_str("# TYPE oracle_token_source_outlier_rejections_total counter\n");
        for (labels, count) in self
            .token_source_outlier_rejections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
        {
            output.push_str(&format!(
                "oracle_token_source_outlier_rejections_total{{symbol=\"{}\",token=\"{}\",source=\"{}\"}} {}\n",
                escape_label_value(&labels.symbol),
                escape_label_value(&labels.token),
                escape_label_value(&labels.source),
                count
            ));
        }

        output.push_str("# HELP oracle_submit_failures_total Total number of submit failures\n");
        output.push_str("# TYPE oracle_submit_failures_total counter\n");
        output.push_str(&format!(
            "oracle_submit_failures_total {}\n",
            c.submit_failures
        ));

        output.push_str("# HELP oracle_last_cycle_metrics_update Timestamp of last price/keeper cycle metrics update\n");
        output.push_str("# TYPE oracle_last_cycle_metrics_update gauge\n");
        output.push_str(&format!(
            "oracle_last_cycle_metrics_update {}\n",
            c.last_metrics_update
        ));

        output.push_str("# HELP oracle_http_requests_total Total number of HTTP requests\n");
        output.push_str("# TYPE oracle_http_requests_total counter\n");
        for (labels, count) in &c.http_requests_total {
            output.push_str(&format!(
                "oracle_http_requests_total{{route=\"{}\",method=\"{}\",status_class=\"{}\"}} {}\n",
                escape_label_value(&labels.route),
                escape_label_value(&labels.method),
                escape_label_value(&labels.status_class),
                count
            ));
        }

        output.push_str(
            "# HELP oracle_http_request_duration_seconds HTTP request duration histogram\n",
        );
        output.push_str("# TYPE oracle_http_request_duration_seconds histogram\n");
        for (route, buckets) in &c.http_request_duration_buckets {
            let escaped_route = escape_label_value(route);
            output.push_str(&format!(
                "oracle_http_request_duration_seconds_bucket{{route=\"{}\",le=\"0.01\"}} {}\n",
                escaped_route, buckets[0]
            ));
            output.push_str(&format!(
                "oracle_http_request_duration_seconds_bucket{{route=\"{}\",le=\"0.05\"}} {}\n",
                escaped_route, buckets[1]
            ));
            output.push_str(&format!(
                "oracle_http_request_duration_seconds_bucket{{route=\"{}\",le=\"0.1\"}} {}\n",
                escaped_route, buckets[2]
            ));
            output.push_str(&format!(
                "oracle_http_request_duration_seconds_bucket{{route=\"{}\",le=\"0.25\"}} {}\n",
                escaped_route, buckets[3]
            ));
            output.push_str(&format!(
                "oracle_http_request_duration_seconds_bucket{{route=\"{}\",le=\"0.5\"}} {}\n",
                escaped_route, buckets[4]
            ));
            output.push_str(&format!(
                "oracle_http_request_duration_seconds_bucket{{route=\"{}\",le=\"1\"}} {}\n",
                escaped_route, buckets[5]
            ));
            output.push_str(&format!(
                "oracle_http_request_duration_seconds_bucket{{route=\"{}\",le=\"+Inf\"}} {}\n",
                escaped_route, buckets[6]
            ));

            output.push_str(&format!(
                "oracle_http_request_duration_seconds_count{{route=\"{}\"}} {}\n",
                escaped_route, buckets[6]
            ));
            if let Some(sum) = c.http_request_duration_sum.get(route) {
                output.push_str(&format!(
                    "oracle_http_request_duration_seconds_sum{{route=\"{}\"}} {}\n",
                    escaped_route, sum
                ));
            }
        }

        output.push_str(
            "# HELP oracle_http_requests_in_flight Current number of in-flight HTTP requests\n",
        );
        output.push_str("# TYPE oracle_http_requests_in_flight gauge\n");
        output.push_str(&format!(
            "oracle_http_requests_in_flight {}\n",
            c.http_requests_in_flight
        ));

        output.push_str("# HELP oracle_http_auth_failures_total Total number of unauthorized admin access attempts\n");
        output.push_str("# TYPE oracle_http_auth_failures_total counter\n");
        for (route, count) in &c.http_auth_failures_total {
            output.push_str(&format!(
                "oracle_http_auth_failures_total{{route=\"{}\"}} {}\n",
                escape_label_value(route),
                count
            ));
        }

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
        assert!(prometheus.contains("oracle_price_cycles_total 1"));
        assert!(prometheus.contains("oracle_price_cycle_duration_seconds 0.1"));
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

    #[test]
    fn test_http_request_metrics_bounded_cardinality() {
        let metrics = Metrics::new();
        // Even if we record multiple unknown paths, they should only use the route provided.
        // In our axum layer we use MatchedPath, or "/unmatched" fallback.
        metrics.record_http_request("/unmatched", "GET", 404, 15);
        metrics.record_http_request("/unmatched", "GET", 404, 25);

        let output = metrics.to_prometheus();
        let matches = output.matches("oracle_http_requests_total").count();
        // 2 matches: one for the HELP/TYPE header, one for the actual metric.
        assert_eq!(matches, 3);
        assert!(output.contains("oracle_http_requests_total{route=\"/unmatched\",method=\"GET\",status_class=\"4xx\"} 2"));
    }
}
