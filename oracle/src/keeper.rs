use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::stellar_rpc::{get_account_balance_stroops, RpcError};

/// 1 XLM expressed in stroops.
pub const XLM_IN_STROOPS: i64 = 10_000_000;

/// Default minimum keeper balance: 10 XLM.
pub const DEFAULT_MIN_KEEPER_BALANCE_XLM: f64 = 10.0;

pub struct KeeperBalanceConfig {
    pub horizon_url: String,
    pub account_id: String,
    /// Minimum acceptable balance in XLM.
    pub min_balance_xlm: f64,
}

/// Default retry attempts for transient keeper balance check failures.
pub const KEEPER_BALANCE_RETRY_ATTEMPTS: u32 = 3;
/// Base backoff delay in milliseconds for transient keeper balance check retries.
pub const KEEPER_BALANCE_RETRY_BASE_DELAY_MS: u64 = 100;

/// Check the keeper balance with retries for transient RPC/network errors.
///
/// Returns the current balance in stroops on success.
/// Non-transient errors like `BalanceBelowMinimum` are returned immediately without retrying.
pub async fn check_keeper_balance_with_retry(
    cfg: &KeeperBalanceConfig,
    below_min: &Arc<AtomicBool>,
) -> Result<i64, RpcError> {
    let mut last_error = None;

    for attempt in 1..=KEEPER_BALANCE_RETRY_ATTEMPTS {
        match check_keeper_balance(cfg, below_min).await {
            Ok(stroops) => return Ok(stroops),
            Err(error @ RpcError::BalanceBelowMinimum { .. }) => {
                return Err(error);
            }
            Err(error) => {
                tracing::warn!(
                    attempt,
                    max_attempts = KEEPER_BALANCE_RETRY_ATTEMPTS,
                    error = %error,
                    "keeper balance check attempt failed"
                );
                last_error = Some(error);
                if attempt < KEEPER_BALANCE_RETRY_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_millis(
                        KEEPER_BALANCE_RETRY_BASE_DELAY_MS * 2_u64.pow(attempt - 1),
                    ))
                    .await;
                }
            }
        }
    }

    Err(last_error.expect("KEEPER_BALANCE_RETRY_ATTEMPTS is greater than zero"))
}

/// Check the keeper balance.  Returns the current balance in stroops.
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
) -> Result<i64, RpcError> {
    let stroops = get_account_balance_stroops(&cfg.horizon_url, &cfg.account_id).await?;

    let xlm = stroops as f64 / XLM_IN_STROOPS as f64;
    if xlm < cfg.min_balance_xlm {
        let was_below = below_min.swap(true, Ordering::Relaxed);
        if was_below {
            tracing::debug!(
                balance_xlm = xlm,
                min_balance_xlm = cfg.min_balance_xlm,
                account_id = cfg.account_id,
                "keeper balance still below minimum"
            );
        } else {
            tracing::error!(
                balance_xlm = xlm,
                min_balance_xlm = cfg.min_balance_xlm,
                account_id = cfg.account_id,
                "keeper balance below minimum"
            );
        }
        return Err(RpcError::BalanceBelowMinimum {
            balance_stroops: stroops,
            balance_xlm: xlm,
            min_xlm: cfg.min_balance_xlm,
        });
    }

    if below_min.swap(false, Ordering::Relaxed) {
        tracing::info!(
            balance_xlm = xlm,
            min_balance_xlm = cfg.min_balance_xlm,
            "keeper balance recovered above minimum"
        );
    } else {
        tracing::debug!(
            balance_xlm = xlm,
            min_balance_xlm = cfg.min_balance_xlm,
            "keeper balance ok"
        );
    }

    Ok(stroops)
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

pub fn build_balance_response(cfg: &KeeperBalanceConfig, stroops: i64) -> BalanceResponse {
    let xlm = stroops as f64 / XLM_IN_STROOPS as f64;
    BalanceResponse {
        account_id: cfg.account_id.clone(),
        balance_stroops: stroops,
        balance_xlm: xlm,
        below_minimum: xlm < cfg.min_balance_xlm,
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

    #[test]
    fn below_minimum_detected() {
        let stroops = 30_000_000i64; // 3 XLM
        let cfg = KeeperBalanceConfig {
            horizon_url: "https://horizon-testnet.stellar.org".to_string(),
            account_id: "GABC".to_string(),
            min_balance_xlm: 10.0,
        };
        let resp = build_balance_response(&cfg, stroops);
        assert!(resp.below_minimum);
        assert_eq!(resp.balance_xlm, 3.0);
    }

    #[test]
    fn above_minimum_not_flagged() {
        let stroops = 200_000_000i64; // 20 XLM
        let cfg = KeeperBalanceConfig {
            horizon_url: "https://horizon-testnet.stellar.org".to_string(),
            account_id: "GABC".to_string(),
            min_balance_xlm: 10.0,
        };
        let resp = build_balance_response(&cfg, stroops);
        assert!(!resp.below_minimum);
        assert_eq!(resp.balance_xlm, 20.0);
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
        let stroops = check_keeper_balance(&cfg, &below_min).await.unwrap();
        assert_eq!(stroops, 200_000_000); // 20 XLM in stroops
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

    #[tokio::test]
    async fn check_keeper_balance_with_retry_succeeds_after_transient_failure() {
        use std::sync::atomic::AtomicUsize;
        use wiremock::matchers::path;
        use wiremock::Request as WireMockRequest;

        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_mock = Arc::clone(&attempts);

        Mock::given(method("GET"))
            .and(path("/accounts/GKEEPER"))
            .respond_with(move |_req: &WireMockRequest| {
                let attempt = attempts_for_mock.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    ResponseTemplate::new(500).set_body_string("transient horizon error")
                } else {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "id": "GKEEPER",
                        "balances": [{"asset_type": "native", "balance": "25.0000000"}]
                    }))
                }
            })
            .mount(&server)
            .await;

        let cfg = KeeperBalanceConfig {
            horizon_url: server.uri(),
            account_id: "GKEEPER".to_string(),
            min_balance_xlm: 10.0,
        };

        let below_min = Arc::new(AtomicBool::new(false));
        let stroops = check_keeper_balance_with_retry(&cfg, &below_min).await.unwrap();
        assert_eq!(stroops, 250_000_000);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn check_keeper_balance_with_retry_fails_immediately_on_below_minimum() {
        use std::sync::atomic::AtomicUsize;
        use wiremock::matchers::path;
        use wiremock::Request as WireMockRequest;

        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_mock = Arc::clone(&attempts);

        Mock::given(method("GET"))
            .and(path("/accounts/GKEEPER"))
            .respond_with(move |_req: &WireMockRequest| {
                attempts_for_mock.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "GKEEPER",
                    "balances": [{"asset_type": "native", "balance": "3.0000000"}]
                }))
            })
            .mount(&server)
            .await;

        let cfg = KeeperBalanceConfig {
            horizon_url: server.uri(),
            account_id: "GKEEPER".to_string(),
            min_balance_xlm: 10.0,
        };

        let below_min = Arc::new(AtomicBool::new(false));
        let err = check_keeper_balance_with_retry(&cfg, &below_min).await.unwrap_err();
        assert!(matches!(err, RpcError::BalanceBelowMinimum { .. }));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
