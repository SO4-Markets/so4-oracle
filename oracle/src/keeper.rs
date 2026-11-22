use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::stellar_rpc::{get_account_balance, RpcError};

/// 1 XLM expressed in stroops.
pub const XLM_IN_STROOPS: i64 = 10_000_000;

/// Stellar's base reserve per ledger entry, in stroops: 0.5 XLM (#1446).
///
/// This is the network's `baseReserve` ledger-header parameter. It is not a
/// protocol constant — it can change by validator vote — but it has been
/// 0.5 XLM since protocol 10, so it is kept as a named constant here rather
/// than fetched on every balance check.
pub const BASE_RESERVE_PER_ENTRY_STROOPS: i64 = 5_000_000;

/// Default minimum keeper balance: 10 XLM.
pub const DEFAULT_MIN_KEEPER_BALANCE_XLM: f64 = 10.0;

pub struct KeeperBalanceConfig {
    pub horizon_url: String,
    pub account_id: String,
    /// Minimum acceptable spendable balance in XLM.
    pub min_balance_xlm: f64,
}

impl KeeperBalanceConfig {
    /// The minimum as exact stroops, so the threshold check is an integer
    /// comparison rather than a float one (#1445).
    pub fn min_balance_stroops(&self) -> i64 {
        xlm_to_stroops(self.min_balance_xlm)
    }
}

/// Converts an XLM amount (as configured) to the nearest whole stroop.
pub fn xlm_to_stroops(xlm: f64) -> i64 {
    (xlm * XLM_IN_STROOPS as f64).round() as i64
}

/// Converts stroops to XLM. Only for display and JSON output — every
/// decision is made on stroops (#1445).
pub fn stroops_to_xlm(stroops: i64) -> f64 {
    stroops as f64 / XLM_IN_STROOPS as f64
}

/// The account's locked minimum balance: `(2 + subentries) × base reserve`
/// (#1446). Every trustline, offer, data entry or extra signer on the keeper
/// account is a subentry and raises it.
pub fn reserve_stroops(subentry_count: u32) -> i64 {
    (2 + i64::from(subentry_count)) * BASE_RESERVE_PER_ENTRY_STROOPS
}

/// Spendable balance once the locked reserve is subtracted (#1215). The one
/// place this arithmetic lives (#1447).
pub fn spendable_stroops(total_stroops: i64, subentry_count: u32) -> i64 {
    total_stroops.saturating_sub(reserve_stroops(subentry_count))
}

/// The keeper's balance: the on-chain total and the part of it that is
/// actually spendable once the account reserve is locked away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeeperBalance {
    pub total_stroops: i64,
    pub spendable_stroops: i64,
}

/// Check the keeper balance against the configured minimum.
///
/// Logs `error!` only on the transition into the low-balance state (and
/// `info!` on recovery). Subsequent checks while the balance remains low
/// use `debug!` so readiness probes do not flood logs.
///
/// The `below_min` flag is scoped to the application instance rather than
/// a bare process-global, so concurrent callers (e.g. /ready and
/// /keeper/balance) share well-defined state (#737).
pub async fn check_keeper_balance(
    cfg: &KeeperBalanceConfig,
    below_min: &Arc<AtomicBool>,
) -> Result<KeeperBalance, RpcError> {
    let account = get_account_balance(&cfg.horizon_url, &cfg.account_id).await?;
    let balance = KeeperBalance {
        total_stroops: account.balance_stroops,
        spendable_stroops: spendable_stroops(account.balance_stroops, account.subentry_count),
    };
    let spendable_xlm = stroops_to_xlm(balance.spendable_stroops);

    if balance.spendable_stroops < cfg.min_balance_stroops() {
        let was_below = below_min.swap(true, Ordering::Relaxed);
        if was_below {
            tracing::debug!(
                event = "keeper_balance_still_low",
                balance_xlm = spendable_xlm,
                min_balance_xlm = cfg.min_balance_xlm,
                account_id = cfg.account_id,
                "keeper balance still below minimum"
            );
        } else {
            tracing::error!(
                event = "keeper_balance_low",
                balance_xlm = spendable_xlm,
                min_balance_xlm = cfg.min_balance_xlm,
                account_id = cfg.account_id,
                "keeper balance below minimum"
            );
        }
        return Err(RpcError::BalanceBelowMinimum {
            balance_stroops: balance.total_stroops,
            balance_xlm: spendable_xlm,
            min_xlm: cfg.min_balance_xlm,
        });
    }

    if below_min.swap(false, Ordering::Relaxed) {
        tracing::info!(
            event = "keeper_balance_recovered",
            balance_xlm = spendable_xlm,
            min_balance_xlm = cfg.min_balance_xlm,
            "keeper balance recovered above minimum"
        );
    } else {
        tracing::debug!(
            balance_xlm = spendable_xlm,
            min_balance_xlm = cfg.min_balance_xlm,
            "keeper balance ok"
        );
    }

    Ok(balance)
}

/// JSON-serialisable balance response for the HTTP endpoint.
#[derive(serde::Serialize)]
pub struct BalanceResponse {
    pub account_id: String,
    pub balance_stroops: i64,
    pub balance_xlm: f64,
    pub below_minimum: bool,
    pub min_balance_xlm: f64,
}

pub fn build_balance_response(
    cfg: &KeeperBalanceConfig,
    balance: KeeperBalance,
) -> BalanceResponse {
    BalanceResponse {
        account_id: cfg.account_id.clone(),
        balance_stroops: balance.total_stroops,
        balance_xlm: stroops_to_xlm(balance.spendable_stroops),
        below_minimum: balance.spendable_stroops < cfg.min_balance_stroops(),
        min_balance_xlm: cfg.min_balance_xlm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stellar_rpc::parse_account_balance_response;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn low_balance_body() -> &'static str {
        r#"{"id":"GABC","balances":[{"asset_type":"native","balance":"3.0000000"}]}"#
    }

    #[test]
    fn parse_low_balance_from_mocked_rpc() {
        let stroops = parse_account_balance_response(low_balance_body()).unwrap();
        assert_eq!(stroops, 30_000_000); // 3 XLM in stroops
    }

    fn cfg(min_balance_xlm: f64) -> KeeperBalanceConfig {
        KeeperBalanceConfig {
            horizon_url: "https://horizon-testnet.stellar.org".to_string(),
            account_id: "GABC".to_string(),
            min_balance_xlm,
        }
    }

    fn balance(total_stroops: i64, subentry_count: u32) -> KeeperBalance {
        KeeperBalance {
            total_stroops,
            spendable_stroops: spendable_stroops(total_stroops, subentry_count),
        }
    }

    #[test]
    fn below_minimum_detected() {
        let resp = build_balance_response(&cfg(10.0), balance(30_000_000, 0)); // 3 XLM
        assert!(resp.below_minimum);
        assert_eq!(resp.balance_stroops, 30_000_000);
        // Spendable: 3 XLM minus the 1 XLM reserve of an account with no subentries.
        assert_eq!(resp.balance_xlm, 2.0);
    }

    #[test]
    fn above_minimum_not_flagged() {
        let resp = build_balance_response(&cfg(10.0), balance(200_000_000, 0)); // 20 XLM
        assert!(!resp.below_minimum);
        assert_eq!(resp.balance_xlm, 19.0);
    }

    // #1446 — the reserve is (2 + subentries) × 0.5 XLM, not a fixed 1 XLM.
    #[test]
    fn reserve_grows_with_subentries() {
        assert_eq!(reserve_stroops(0), 10_000_000);
        assert_eq!(reserve_stroops(1), 15_000_000);
        assert_eq!(reserve_stroops(4), 30_000_000);
        assert_eq!(spendable_stroops(100_000_000, 1), 85_000_000);
    }

    // #1445 — the threshold is an exact integer comparison: spendable equal to
    // the minimum is not below it, one stroop less is.
    #[test]
    fn minimum_boundary_is_exact_in_stroops() {
        let reserve = reserve_stroops(0);
        let at_min = build_balance_response(&cfg(10.0), balance(100_000_000 + reserve, 0));
        assert!(!at_min.below_minimum);
        let one_short = build_balance_response(&cfg(10.0), balance(99_999_999 + reserve, 0));
        assert!(one_short.below_minimum);
    }

    #[test]
    fn xlm_and_stroops_convert_exactly_for_configured_values() {
        assert_eq!(xlm_to_stroops(10.0), 100_000_000);
        assert_eq!(xlm_to_stroops(0.1), 1_000_000);
        assert_eq!(xlm_to_stroops(0.0000001), 1);
        assert_eq!(stroops_to_xlm(25_000_000), 2.5);
    }

    // ── check_keeper_balance — HTTP-level tests (#406) ────────────────────────

    /// Closes #414: above minimum returns Ok(stroops).
    #[tokio::test]
    async fn check_keeper_balance_above_minimum_returns_ok() {
        use wiremock::matchers::path;

        let server = MockServer::start().await;
        let body = r#"{
            "id": "GKEEPER",
            "balances": [{"asset_type":"native","balance":"20.0000000"}]
        }"#;

        Mock::given(method("GET"))
            .and(path("/accounts/GKEEPER"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let cfg = KeeperBalanceConfig {
            horizon_url: server.uri(),
            account_id: "GKEEPER".to_string(),
            min_balance_xlm: 10.0,
        };

        let below_min = Arc::new(AtomicBool::new(false));
        let balance = check_keeper_balance(&cfg, &below_min).await.unwrap();
        assert_eq!(balance.total_stroops, 200_000_000); // 20 XLM in stroops
        assert_eq!(balance.spendable_stroops, 190_000_000);
    }

    // #1446 — the subentry count from Horizon's account response raises the
    // reserve, so a keeper with subentries has less spendable balance.
    #[tokio::test]
    async fn check_keeper_balance_uses_horizon_subentry_count() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "subentry_count": 3,
                "balances": [{"asset_type": "native", "balance": "12.0000000"}]
            })))
            .mount(&server)
            .await;

        let cfg = KeeperBalanceConfig {
            horizon_url: server.uri(),
            account_id: "GKEEPER".to_string(),
            min_balance_xlm: 10.0,
        };

        // 12 XLM total − (2 + 3) × 0.5 XLM reserve = 9.5 XLM spendable < 10.
        let below_min = Arc::new(AtomicBool::new(false));
        let err = check_keeper_balance(&cfg, &below_min).await.unwrap_err();
        assert!(matches!(
            err,
            RpcError::BalanceBelowMinimum {
                balance_stroops: 120_000_000,
                ..
            }
        ));
    }

    /// Closes #413: below minimum returns Err(BalanceBelowMinimum).
    #[tokio::test]
    async fn check_keeper_balance_below_minimum_returns_err() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "balances": [{"asset_type": "native", "balance": "3.0000000"}]
            })))
            .mount(&server)
            .await;

        let cfg = KeeperBalanceConfig {
            horizon_url: server.uri(),
            account_id: "GKEEPER".to_string(),
            min_balance_xlm: 10.0,
        };

        let below_min = Arc::new(AtomicBool::new(false));
        let err = check_keeper_balance(&cfg, &below_min).await.unwrap_err();
        assert!(matches!(err, RpcError::BalanceBelowMinimum { .. }));
    }

    /// Horizon unreachable returns NetworkError.
    #[tokio::test]
    async fn check_keeper_balance_horizon_unreachable_returns_network_error() {
        let cfg = KeeperBalanceConfig {
            horizon_url: "http://127.0.0.1:19999".to_string(), // nothing listening
            account_id: "GKEEPER".to_string(),
            min_balance_xlm: 10.0,
        };

        let below_min = Arc::new(AtomicBool::new(false));
        let err = check_keeper_balance(&cfg, &below_min).await.unwrap_err();
        assert!(matches!(err, RpcError::NetworkError(_)));
    }
}
