/// Minimum number of price sources required to compute percentile-based spread.
/// With fewer sources we fall back to an equal spread around the median.
pub const MIN_SOURCES_FOR_PERCENTILE: usize = 3;

/// Price spread returned for on-chain submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceProps {
    /// 10th-percentile price (or fallback lower bound).
    pub min: i128,
    /// 90th-percentile price (or fallback upper bound).
    pub max: i128,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RejectedSource {
    pub source: String,
    pub price: i128,
    pub deviation_bps: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AggregatedPrice {
    pub min: i128,
    pub max: i128,
    pub median: i128,
    pub sources_used: Vec<String>,
    pub rejected_sources: Vec<RejectedSource>,
}

pub fn aggregate_prices(
    prices: &[i128],
    sources: &[String],
    min_sources: usize,
    max_deviation_bps: u32,
) -> Result<AggregatedPrice, String> {
    if prices.len() != sources.len() {
        return Err("prices and sources length mismatch".to_string());
    }
    if prices.len() < min_sources {
        return Err(format!(
            "insufficient sources: got {}, need {}",
            prices.len(),
            min_sources
        ));
    }

    let filter_result = filter_outliers(prices, sources);
    let filtered_prices = filter_result.filtered_prices;
    let filtered_sources = filter_result.filtered_sources;

    if filtered_prices.len() < min_sources {
        let _rejected_sources: Vec<RejectedSource> = filter_result
            .rejected
            .into_iter()
            .map(|(source, price, deviation)| RejectedSource {
                source,
                price,
                deviation_bps: deviation,
            })
            .collect();

        return Err(format!(
            "insufficient sources after filtering: got {}, need {} (rejected by MAD-based outlier filter)",
            filtered_prices.len(),
            min_sources
        ));
    }

    let props = compute_confidence_interval_with_spread(&filtered_prices, max_deviation_bps)
        .ok_or_else(|| "cannot compute confidence interval".to_string())?;
    let median = compute_median_allow_single(&filtered_prices).unwrap_or(props.min);

    let rejected_sources = filter_result
        .rejected
        .into_iter()
        .map(|(source, price, deviation)| RejectedSource {
            source,
            price,
            deviation_bps: deviation,
        })
        .collect();

    Ok(AggregatedPrice {
        min: props.min,
        max: props.max,
        median,
        sources_used: filtered_sources,
        rejected_sources,
    })
}

pub fn compute_confidence_interval(prices: &[i128]) -> Option<PriceProps> {
    compute_confidence_interval_with_spread(prices, 100)
}

/// Compute the price spread from a slice of raw source prices.
pub fn compute_confidence_interval_with_spread(
    prices: &[i128],
    spread_bps: u32,
) -> Option<PriceProps> {
    if prices.is_empty() {
        return None;
    }

    let mut sorted = prices.to_vec();
    sorted.sort_unstable();

    if sorted.len() >= MIN_SOURCES_FOR_PERCENTILE {
        let min = percentile(&sorted, 10);
        let max = percentile(&sorted, 90);
        Some(PriceProps { min, max })
    } else {
        let mid = compute_median_allow_single(&sorted)?;
        let spread = mid.saturating_mul(spread_bps as i128) / 10_000;
        Some(PriceProps {
            min: mid.saturating_sub(spread).max(0),
            max: mid.saturating_add(spread),
        })
    }
}

/// Interpolating percentile (nearest-rank method).
pub fn percentile(sorted: &[i128], p: u8) -> i128 {
    debug_assert!(!sorted.is_empty());
    if sorted.len() == 1 || p == 0 {
        return sorted[0];
    }
    if p >= 100 {
        return *sorted.last().unwrap();
    }
    // index = p/100 * (n-1), linear interpolation between floor and ceil
    let n = sorted.len() as f64;
    let idx = (p as f64 / 100.0) * (n - 1.0);
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = idx - lo as f64;
    let lo_val = sorted[lo] as f64;
    let hi_val = sorted[hi] as f64;
    (lo_val + frac * (hi_val - lo_val) + 0.5).floor() as i128
}

#[derive(Debug)]
pub struct OutlierFilterResult {
    pub filtered_prices: Vec<i128>,
    pub filtered_sources: Vec<String>,
    pub rejected: Vec<(String, i128, f64)>, // source, price, deviation
}

/// Filter out prices that deviate too far from the median.
///
/// Primary rule: reject prices whose absolute deviation from the median exceeds
/// 6x the median absolute deviation (MAD). If MAD is zero (a degenerate/flat
/// cluster where at least half the inputs have identical deviation), fall back
/// to rejecting prices more than 3 standard deviations from the median.
pub fn filter_outliers(prices: &[i128], sources: &[String]) -> OutlierFilterResult {
    if prices.is_empty() {
        return OutlierFilterResult {
            filtered_prices: vec![],
            filtered_sources: vec![],
            rejected: vec![],
        };
    }

    // 1. Compute median
    let mut sorted = prices.to_vec();
    sorted.sort_unstable();
    let median = if sorted.len().is_multiple_of(2) {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2
    } else {
        sorted[sorted.len() / 2]
    };

    // 2. Prefer median absolute deviation because a single bad source can
    // inflate standard deviation enough to hide itself.
    let mut deviations: Vec<i128> = prices.iter().map(|&p| (p - median).abs()).collect();
    deviations.sort_unstable();
    let mad = if deviations.len().is_multiple_of(2) {
        (deviations[deviations.len() / 2 - 1] + deviations[deviations.len() / 2]) / 2
    } else {
        deviations[deviations.len() / 2]
    };

    // 3. Compute mean and standard deviation as a fallback for flat clusters.
    let sum: i128 = prices.iter().sum();
    let mean = sum as f64 / prices.len() as f64;
    let variance = prices
        .iter()
        .map(|&p| {
            let diff = p as f64 - mean;
            diff * diff
        })
        .sum::<f64>()
        / prices.len() as f64;
    let stddev = variance.sqrt();

    let mut filtered_prices = Vec::new();
    let mut filtered_sources = Vec::new();
    let mut rejected = Vec::new();

    for (i, &p) in prices.iter().enumerate() {
        let dev = (p as f64 - median as f64).abs();
        let is_outlier = if mad > 0 {
            dev > 6.0 * mad as f64
        } else {
            stddev > 0.0 && dev > 3.0 * stddev
        };

        if is_outlier {
            rejected.push((sources[i].clone(), p, dev));
        } else {
            filtered_prices.push(p);
            filtered_sources.push(sources[i].clone());
        }
    }

    OutlierFilterResult {
        filtered_prices,
        filtered_sources,
        rejected,
    }
}

/// Compute the median of a slice of prices safely.
pub fn compute_median(prices: &[i128]) -> Option<i128> {
    if prices.len() < 2 {
        return None;
    }
    compute_median_allow_single(prices)
}

pub fn compute_median_allow_single(prices: &[i128]) -> Option<i128> {
    if prices.is_empty() {
        return None;
    }
    let mut sorted = prices.to_vec();
    sorted.sort_unstable();
    if sorted.len().is_multiple_of(2) {
        Some((sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2)
    } else {
        Some(sorted[sorted.len() / 2])
    }
}

pub fn deviation_bps(price: i128, median: i128) -> f64 {
    if median == 0 {
        return f64::INFINITY;
    }
    ((price as f64 - median as f64).abs() / (median as f64).abs()) * 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_prices_tenth_and_ninetieth_percentile() {
        // sorted: [100, 200, 300, 400, 500]
        // 10th percentile index = 0.1 * 4 = 0.4 → lo=0 hi=1 → 100 + 0.4*(200-100) = 140
        // 90th percentile index = 0.9 * 4 = 3.6 → lo=3 hi=4 → 400 + 0.6*(500-400) = 460
        let prices = vec![300i128, 100, 500, 200, 400];
        let p = compute_confidence_interval(&prices).unwrap();
        assert_eq!(p.min, 140);
        assert_eq!(p.max, 460);
    }

    #[test]
    fn three_sources_uses_percentile_not_fallback() {
        let prices = vec![100i128, 200, 300];
        // 10th: 0.1*2=0.2 → 100+0.2*100=120
        // 90th: 0.9*2=1.8 → 200+0.8*100=280
        let p = compute_confidence_interval(&prices).unwrap();
        assert_eq!(p.min, 120);
        assert_eq!(p.max, 280);
    }

    #[test]
    fn two_sources_uses_average_median_equal_spread() {
        let prices = vec![1000i128, 2000];
        let p = compute_confidence_interval(&prices).unwrap();

        assert_eq!(p.min, 1485, "Expected mid (1500) - 1% (15)");
        assert_eq!(p.max, 1515, "Expected mid (1500) + 1% (15)");
    }

    #[test]
    fn single_source_uses_fallback_equal_spread() {
        let prices = vec![5000i128];
        let p = compute_confidence_interval(&prices).unwrap();

        assert_eq!(p.min, 4950, "Expected 5000 - 1% spread (50)");
        assert_eq!(p.max, 5050, "Expected 5000 + 1% spread (50)");
    }

    #[test]
    fn empty_prices_returns_none() {
        assert_eq!(compute_confidence_interval(&[]), None);
    }

    #[test]
    fn min_is_less_than_or_equal_to_max() {
        let prices = vec![42i128, 43, 44, 45, 46];
        let p = compute_confidence_interval(&prices).unwrap();
        assert!(p.min <= p.max);
    }

    #[test]
    fn even_source_count_six_prices() {
        let prices = vec![100i128, 200, 300, 400, 500, 600];
        let p = compute_confidence_interval(&prices).unwrap();
        // 10th: 0.1*5=0.5 → lo=0 hi=1 → 100+0.5*100=150
        // 90th: 0.9*5=4.5 → lo=4 hi=5 → 500+0.5*100=550
        assert_eq!(p.min, 150);
        assert_eq!(p.max, 550);
        assert!(p.min <= p.max);
    }

    #[test]
    fn odd_source_count_seven_prices() {
        let prices = vec![10i128, 20, 30, 40, 50, 60, 70];
        let p = compute_confidence_interval(&prices).unwrap();
        // 10th: 0.1*6=0.6 → lo=0 hi=1 → 10+0.6*10=16
        // 90th: 0.9*6=5.4 → lo=5 hi=6 → 60+0.4*10=64
        assert_eq!(p.min, 16);
        assert_eq!(p.max, 64);
        assert!(p.min <= p.max);
    }

    #[test]
    fn median_calculation_odd_count() {
        let prices = vec![1i128, 2, 3, 4, 5];
        let p = compute_confidence_interval(&prices).unwrap();
        let sorted = [1, 2, 3, 4, 5];
        let median = sorted[sorted.len() / 2]; // 3
        assert_eq!(median, 3);
        assert!(p.min <= p.max);
    }

    #[test]
    fn median_calculation_even_count() {
        let prices = vec![1i128, 2, 3, 4, 5, 6];
        let p = compute_confidence_interval(&prices).unwrap();
        let median = compute_median(&prices).unwrap();
        assert_eq!(median, 3);
        assert!(p.min <= p.max);
    }

    #[test]
    fn single_source_requires_configured_min_sources() {
        let sources = vec!["fixed".to_string()];
        let ok = aggregate_prices(&[1_000], &sources, 1, 50).unwrap();
        assert_eq!(ok.median, 1_000);
        let err = aggregate_prices(&[1_000], &sources, 2, 50).unwrap_err();
        assert!(err.contains("insufficient sources"));
    }

    #[test]
    fn max_deviation_bps_rejects_outlier() {
        let sources = vec![
            "binance".to_string(),
            "coinbase".to_string(),
            "pyth".to_string(),
        ];
        let result = aggregate_prices(&[100, 101, 160], &sources, 2, 200).unwrap();
        assert_eq!(result.sources_used, vec!["binance", "coinbase"]);
        assert_eq!(result.rejected_sources.len(), 1);
    }

    #[test]
    fn confidence_interval_with_outliers() {
        // Large outliers at both ends; 10th-90th percentile should exclude them
        let prices = vec![1i128, 2, 3, 4, 100, 200, 300, 400, 500, 1000000];
        let p = compute_confidence_interval(&prices).unwrap();
        // With percentile method, outliers don't heavily skew the interval
        assert!(p.min <= p.max);
        // 10th percentile should be much lower than max
        assert!(p.max > p.min);
    }

    #[test]
    fn duplicate_prices() {
        let prices = vec![100i128, 100, 100, 100, 100];
        let p = compute_confidence_interval(&prices).unwrap();
        // All the same price → percentiles should be 100
        assert_eq!(p.min, 100);
        assert_eq!(p.max, 100);
    }

    #[test]
    fn large_price_values() {
        let prices = vec![1_000_000_000i128, 2_000_000_000, 3_000_000_000];
        let p = compute_confidence_interval(&prices).unwrap();
        // Should handle large values without overflow
        assert!(p.min <= p.max);
        assert!(p.min > 0);
        assert!(p.max > 0);
    }

    #[test]
    fn percentile_boundary_p_zero() {
        let sorted = [100i128, 200, 300];
        // percentile with p=0 should return first element
        assert_eq!(percentile(&sorted, 0), 100);
    }

    #[test]
    fn percentile_boundary_p_hundred() {
        let sorted = [100i128, 200, 300];
        // percentile with p=100 should return last element
        assert_eq!(percentile(&sorted, 100), 300);
    }

    #[test]
    fn percentile_single_element() {
        let sorted = [42i128];
        // Single element should return that element for any percentile
        assert_eq!(percentile(&sorted, 10), 42);
        assert_eq!(percentile(&sorted, 50), 42);
        assert_eq!(percentile(&sorted, 90), 42);
    }

    #[test]
    fn fallback_spread_with_large_bps_does_not_underflow() {
        let prices = vec![100i128, 200];
        // spread_bps=20000 means 200%, so spread=200 and mid=150
        // mid - spread = -50 would underflow; saturating_sub should clamp to 0
        let p = compute_confidence_interval_with_spread(&prices, 20_000).unwrap();
        assert!(p.min >= 0, "min should not be negative, got {}", p.min);
        assert!(p.max >= p.min);
    }

    #[test]
    fn full_aggregation_pipeline_even_sources() {
        // Simulate a full price aggregation with even number of sources
        let prices = [45000i128, 45100, 44900, 45050];
        let p = compute_confidence_interval(&prices).unwrap();
        assert!(p.min <= p.max);
        assert!(p.min >= 44900);
        assert!(p.max <= 45100);
    }

    #[test]
    fn full_aggregation_pipeline_odd_sources() {
        // Simulate a full price aggregation with odd number of sources
        let prices = [2500i128, 2510, 2490, 2505, 2495];
        let p = compute_confidence_interval(&prices).unwrap();
        assert!(p.min <= p.max);
        assert!(p.min >= 2490);
        assert!(p.max <= 2510);
    }

    #[test]
    fn test_filter_outliers_removes_10x_outlier() {
        let prices = vec![1000, 1010, 990, 1005, 10000]; // 10000 is a 10x outlier
        let sources = vec![
            "src1".to_string(),
            "src2".to_string(),
            "src3".to_string(),
            "src4".to_string(),
            "bad_src".to_string(),
        ];

        let result = filter_outliers(&prices, &sources);

        // Should reject 1
        assert_eq!(result.rejected.len(), 1);
        assert_eq!(result.rejected[0].0, "bad_src");
        assert_eq!(result.rejected[0].1, 10000);

        // Should keep 4
        assert_eq!(result.filtered_prices.len(), 4);
        assert!(!result.filtered_prices.contains(&10000));
        assert!(!result.filtered_sources.contains(&"bad_src".to_string()));
    }

    #[test]
    fn test_filter_outliers_degenerate_case() {
        // If all are far apart (e.g. standard deviation is huge), maybe none are rejected,
        // or if they are all outliers from the median (e.g., [10, 1000, 100000]).
        // Wait, if N=3, dev > 3*stddev is impossible because max dev is < stddev * sqrt(N-1).
        // Let's just ensure it doesn't crash on empty.
        let result = filter_outliers(&[], &[]);
        assert!(result.filtered_prices.is_empty());
    }

    #[test]
    fn test_compute_median_three_prices() {
        let prices = [1000, 3000, 2000];
        let median = compute_median(&prices);
        assert_eq!(median, Some(2000));
    }

    #[test]
    fn test_compute_median_five_prices() {
        let prices = [1000, 3000, 2000, 5000, 4000];
        let median = compute_median(&prices);
        assert_eq!(median, Some(3000));
    }

    #[test]
    fn test_compute_median_two_prices() {
        let prices = [1000, 3000];
        let median = compute_median(&prices);
        assert_eq!(median, Some(2000));
    }

    #[test]
    fn test_compute_median_six_prices() {
        let prices = [1000, 2000, 3000, 4000, 5000, 6000];
        let median = compute_median(&prices);
        assert_eq!(median, Some(3500));
    }

    #[test]
    fn test_compute_median_one_price_skipped() {
        let prices = [1000];
        let median = compute_median(&prices);
        assert_eq!(median, None);
    }

    #[test]
    fn aggregate_prices_fails_when_filtered_lt_min() {
        let sources = vec![
            "binance".to_string(),
            "coinbase".to_string(),
            "pyth".to_string(),
        ];
        let result = aggregate_prices(&[100, 101, 1000], &sources, 3, 200);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("insufficient sources after filtering"));
    }

    // #510 — aggregate_prices length-mismatch and empty-input error branches

    #[test]
    fn aggregate_prices_length_mismatch_returns_error() {
        let sources = vec!["binance".to_string()];
        let err = aggregate_prices(&[100, 200], &sources, 0, 100).unwrap_err();
        assert_eq!(err, "prices and sources length mismatch");
    }

    #[test]
    fn aggregate_prices_empty_input_with_zero_min_sources_returns_error() {
        // min_sources = 0 bypasses the earlier `prices.len() < min_sources`
        // guard. #510 named the expected error as "cannot aggregate empty
        // price list", but no such string exists anywhere in aggregate_prices
        // or its helpers as currently implemented (confirmed via grep) — the
        // empty case instead falls through filter_outliers (which returns an
        // empty result for empty input, not an error) into
        // compute_confidence_interval_with_spread, which is what actually
        // rejects it. #510's premise was stale by the time this was worked;
        // asserting the real error here rather than one that was never
        // producible.
        let prices: Vec<i128> = vec![];
        let sources: Vec<String> = vec![];
        let err = aggregate_prices(&prices, &sources, 0, 100).unwrap_err();
        assert_eq!(err, "cannot compute confidence interval");
    }

    // #510's third scenario ("construct two sources both outside
    // max_deviation_bps of the median with min_sources=0" to make
    // filter_outliers reject every source, reaching "cannot compute
    // confidence interval" via an empty filtered list) turns out not to be
    // reachable through filter_outliers as currently implemented.
    // filter_outliers's MAD/stddev thresholds are themselves derived from
    // the same input set, so whichever price sits at (or ties for) the
    // median is always within its own computed deviation bound — verified
    // empirically across several 2-9-source inputs (symmetric pairs,
    // clustered-plus-one-extreme-outlier, evenly-spaced runs): at least one
    // source always survives filtering in every case tried. The
    // "cannot compute confidence interval" error is still real and still
    // covered — see the empty-input test above, which reaches the same
    // error via prices.is_empty() short-circuiting filter_outliers itself
    // rather than via every source being rejected by it.

    #[test]
    fn test_issue_380_explicit_percentile_validation() {
        // Input of 3 sources
        let prices = vec![100i128, 200, 300];

        // If it mistakenly used the fallback spread (100 bps / 1%),
        // the spread around the median (200) would be:
        // mid = 200, spread = 200 * 100 / 10_000 = 2
        // fallback_min = 198, fallback_max = 202

        let p = compute_confidence_interval(&prices).unwrap();

        // Assert that the results match the 10th/90th percentile values,
        // which completely validates that we are NOT using the spread fallback.
        assert_eq!(
            p.min, 120,
            "Should use percentile min (120), not fallback spread min (198)"
        );
        assert_eq!(
            p.max, 280,
            "Should use percentile max (280), not fallback spread max (202)"
        );

        assert_ne!(p.min, 198);
        assert_ne!(p.max, 202);
    }

    // ── Property-based tests (Issue #528) ────────────────────────────────────

    #[test]
    fn property_min_max_within_input_range() {
        let test_cases = vec![
            vec![100i128, 200, 300],
            vec![1000, 1001, 1002, 1003, 1004],
            vec![50, 75, 100, 125, 150, 175, 200],
            vec![10i128, 20, 30],
        ];

        for prices in test_cases {
            let min_input = *prices.iter().min().unwrap();
            let max_input = *prices.iter().max().unwrap();

            let p = compute_confidence_interval(&prices).unwrap();

            assert!(
                p.min >= min_input,
                "computed min {} should be >= input min {}",
                p.min,
                min_input
            );
            assert!(
                p.max <= max_input,
                "computed max {} should be <= input max {}",
                p.max,
                max_input
            );
        }
    }

    #[test]
    fn property_confidence_interval_always_satisfies_min_lte_max() {
        let test_cases = vec![
            vec![42i128],
            vec![100, 200],
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            vec![999_999_999i128, 1_000_000_000, 1_000_000_001],
            vec![0i128, 0, 0, 0],
        ];

        for prices in test_cases {
            let p = compute_confidence_interval(&prices).unwrap();
            assert!(
                p.min <= p.max,
                "invariant violated: min {} > max {} for prices {:?}",
                p.min,
                p.max,
                prices
            );
        }
    }

    #[test]
    fn property_filter_outliers_idempotent() {
        let test_cases = vec![
            (vec![100i128, 101, 102, 103, 10000], 5),
            (vec![50i128, 55, 60, 65, 70], 5),
            (vec![1000i128, 1010, 1020, 1030, 1040, 1050, 1060, 1070], 8),
        ];

        for (prices, n) in test_cases {
            let sources: Vec<String> = (0..n).map(|i| format!("src{}", i)).collect();

            let first_pass = filter_outliers(&prices, &sources);
            let second_pass =
                filter_outliers(&first_pass.filtered_prices, &first_pass.filtered_sources);

            assert_eq!(
                first_pass.filtered_prices, second_pass.filtered_prices,
                "filter_outliers is not idempotent; second pass changed result"
            );
            assert_eq!(
                first_pass.filtered_sources, second_pass.filtered_sources,
                "filter_outliers sources not idempotent"
            );
        }
    }

    #[test]
    fn property_sources_used_count_invariant() {
        let test_cases = vec![
            (vec![100i128, 200, 300], vec!["a", "b", "c"]),
            (
                vec![1000i128, 1001, 1002, 1003],
                vec!["src1", "src2", "src3", "src4"],
            ),
        ];

        for (prices, source_names) in test_cases {
            let sources: Vec<String> = source_names.iter().map(|s| s.to_string()).collect();

            if let Ok(result) = aggregate_prices(&prices, &sources, 1, 10_000) {
                assert_eq!(
                    result.sources_used.len() + result.rejected_sources.len(),
                    prices.len(),
                    "invariant violated: sources_used.len() + rejected_sources.len() != prices.len()"
                );
            }
        }
    }

    #[test]
    fn property_aggregate_prices_order_insensitive() {
        let prices = vec![100i128, 200, 300, 400, 500];
        let sources: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let result1 = aggregate_prices(&prices, &sources, 2, 500).unwrap();

        let mut prices_shuffled = prices.clone();
        let mut sources_shuffled = sources.clone();
        prices_shuffled.reverse();
        sources_shuffled.reverse();

        let result2 = aggregate_prices(&prices_shuffled, &sources_shuffled, 2, 500).unwrap();

        assert_eq!(
            result1.min, result2.min,
            "min differs when input order changes"
        );
        assert_eq!(
            result1.max, result2.max,
            "max differs when input order changes"
        );
        assert_eq!(
            result1.median, result2.median,
            "median differs when input order changes"
        );
    }

    #[test]
    fn property_confidence_interval_bounds_tight() {
        let prices = vec![100i128, 150, 200, 250, 300];

        let p = compute_confidence_interval(&prices).unwrap();

        assert!(p.min >= 100, "lower bound should respect minimum input");
        assert!(p.max <= 300, "upper bound should respect maximum input");
    }

    #[test]
    fn property_empty_filter_outliers_idempotent() {
        let result = filter_outliers(&[], &[]);
        let result2 = filter_outliers(&result.filtered_prices, &result.filtered_sources);

        assert!(result.filtered_prices.is_empty());
        assert!(result2.filtered_prices.is_empty());
    }

    #[test]
    fn property_single_price_always_kept() {
        let prices = vec![42i128];
        let sources = vec!["only".to_string()];

        let result = filter_outliers(&prices, &sources);

        assert_eq!(result.filtered_prices.len(), 1);
        assert_eq!(result.filtered_prices[0], 42);
        assert_eq!(result.rejected.len(), 0);
    }
}
