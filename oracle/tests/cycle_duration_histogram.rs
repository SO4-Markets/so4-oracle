/// Tests for issue #1400: price-cycle and keeper-cycle duration histograms.
///
/// Verifies that:
/// - `record_price_cycle` populates the histogram buckets and sum correctly.
/// - `record_keeper_cycle` populates the histogram buckets and sum correctly.
/// - `to_prometheus` emits histogram lines (bucket, sum, count) for both cycles.
/// - The deprecated last-value gauges are still present for one-release compat.
/// - Bucket counts are cumulative (each bucket counts observations ≤ its bound).
use oracle::metrics::{KeeperCycleTally, Metrics};

#[test]
fn price_cycle_histogram_buckets_are_cumulative() {
    let m = Metrics::new();

    // 300 ms — should land in bucket 0.25? No: 0.3 > 0.25, so first bucket
    // that fits is 0.5 (index 1). All buckets from 0.5 onwards must be 1.
    m.record_price_cycle(300, 2, 0);

    let prom = m.to_prometheus();

    // le="0.25" must be 0 (300 ms > 250 ms)
    assert!(
        prom.contains("oracle_price_cycle_duration_seconds_histogram_bucket{le=\"0.25\"} 0"),
        "0.25-bucket must be 0 for a 300 ms cycle:\n{prom}"
    );
    // le="0.5" must be 1 (300 ms ≤ 500 ms)
    assert!(
        prom.contains("oracle_price_cycle_duration_seconds_histogram_bucket{le=\"0.5\"} 1"),
        "0.5-bucket must be 1 for a 300 ms cycle:\n{prom}"
    );
    // +Inf must be 1
    assert!(
        prom.contains("oracle_price_cycle_duration_seconds_histogram_bucket{le=\"+Inf\"} 1"),
        "+Inf bucket must be 1:\n{prom}"
    );
}

#[test]
fn price_cycle_histogram_sum_and_count() {
    let m = Metrics::new();

    m.record_price_cycle(500, 1, 0); // 0.5 s
    m.record_price_cycle(2000, 1, 0); // 2.0 s

    let prom = m.to_prometheus();

    // sum = 0.5 + 2.0 = 2.5
    assert!(
        prom.contains("oracle_price_cycle_duration_seconds_histogram_sum 2.5"),
        "histogram sum must be 2.5:\n{prom}"
    );
    // count = 2
    assert!(
        prom.contains("oracle_price_cycle_duration_seconds_histogram_count 2"),
        "histogram count must be 2:\n{prom}"
    );
}

#[test]
fn price_cycle_histogram_slow_cycle_in_tail_buckets() {
    let m = Metrics::new();

    // 45 s — lands in the 60-bucket but not 30-bucket.
    m.record_price_cycle(45_000, 1, 0);

    let prom = m.to_prometheus();

    assert!(
        prom.contains("oracle_price_cycle_duration_seconds_histogram_bucket{le=\"30\"} 0"),
        "30-bucket must be 0 for a 45 s cycle:\n{prom}"
    );
    assert!(
        prom.contains("oracle_price_cycle_duration_seconds_histogram_bucket{le=\"60\"} 1"),
        "60-bucket must be 1 for a 45 s cycle:\n{prom}"
    );
}

#[test]
fn keeper_cycle_histogram_buckets_are_cumulative() {
    let m = Metrics::new();

    // 3 s — first bucket that fits is 5.0 (3 > 2.5).
    m.record_keeper_cycle(3_000, KeeperCycleTally::default());

    let prom = m.to_prometheus();

    assert!(
        prom.contains("oracle_keeper_cycle_duration_seconds_histogram_bucket{le=\"2.5\"} 0"),
        "2.5-bucket must be 0 for a 3 s keeper cycle:\n{prom}"
    );
    assert!(
        prom.contains("oracle_keeper_cycle_duration_seconds_histogram_bucket{le=\"5\"} 1"),
        "5-bucket must be 1 for a 3 s keeper cycle:\n{prom}"
    );
    assert!(
        prom.contains("oracle_keeper_cycle_duration_seconds_histogram_bucket{le=\"+Inf\"} 1"),
        "+Inf bucket must be 1:\n{prom}"
    );
}

#[test]
fn keeper_cycle_histogram_sum_and_count() {
    let m = Metrics::new();

    m.record_keeper_cycle(1_000, KeeperCycleTally::default()); // 1 s
    m.record_keeper_cycle(10_000, KeeperCycleTally::default()); // 10 s

    let prom = m.to_prometheus();

    assert!(
        prom.contains("oracle_keeper_cycle_duration_seconds_histogram_sum 11"),
        "keeper histogram sum must be 11:\n{prom}"
    );
    assert!(
        prom.contains("oracle_keeper_cycle_duration_seconds_histogram_count 2"),
        "keeper histogram count must be 2:\n{prom}"
    );
}

#[test]
fn legacy_gauges_still_present() {
    let m = Metrics::new();
    m.record_price_cycle(800, 1, 0);
    m.record_keeper_cycle(3_000, KeeperCycleTally::default());

    let prom = m.to_prometheus();

    assert!(
        prom.contains("oracle_price_cycle_duration_seconds "),
        "deprecated price gauge must still be emitted:\n{prom}"
    );
    assert!(
        prom.contains("oracle_keeper_cycle_duration_seconds "),
        "deprecated keeper gauge must still be emitted:\n{prom}"
    );
}

#[test]
fn prometheus_output_declares_histogram_type() {
    let m = Metrics::new();
    let prom = m.to_prometheus();

    assert!(
        prom.contains("# TYPE oracle_price_cycle_duration_seconds_histogram histogram"),
        "price histogram TYPE line must be present:\n{prom}"
    );
    assert!(
        prom.contains("# TYPE oracle_keeper_cycle_duration_seconds_histogram histogram"),
        "keeper histogram TYPE line must be present:\n{prom}"
    );
}
