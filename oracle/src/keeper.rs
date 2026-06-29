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

/// Check the keeper balance.  Returns the current balance in stroops.
///
/// Logs a critical warning and optionally returns the balance even when below
/// threshold so the caller can decide whether to skip submission.
pub async fn check_keeper_balance(cfg: &KeeperBalanceConfig) -> Result<i64, RpcError> {
    let stroops = get_account_balance_stroops(&cfg.horizon_url, &cfg.account_id).await?;

    let xlm = stroops as f64 / XLM_IN_STROOPS as f64;
    if xlm < cfg.min_balance_xlm {
        tracing::error!(
            balance_xlm = xlm,
            min_balance_xlm = cfg.min_balance_xlm,
            account_id = cfg.account_id,
            "keeper balance below minimum"
        );
        return Err(RpcError::BalanceBelowMinimum {
            balance_xlm: xlm,
            min_xlm: cfg.min_balance_xlm,
        });
    }

    tracing::info!(
        balance_xlm = xlm,
        min_balance_xlm = cfg.min_balance_xlm,
        "keeper balance ok"
    );

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

/// Testnet Friendbot URL base (Issue #120).
pub const FRIENDBOT_URL: &str = "https://friendbot.stellar.org";

/// Call the Stellar testnet Friendbot to fund `account_id`.
pub async fn fund_keeper_via_friendbot(account_id: &str) -> Result<(), String> {
    fund_keeper_at(FRIENDBOT_URL, account_id).await
}

async fn fund_keeper_at(base_url: &str, account_id: &str) -> Result<(), String> {
    let url = format!("{base_url}?addr={account_id}");
    tracing::info!(account_id, "calling Friendbot");

    let response = crate::http::client()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Friendbot fetch failed: {e}"))?;

    let status = response.status().as_u16();
    // 200 = funded; 400 = account already exists (idempotent — treat as success)
    if status == 200 || status == 400 {
        tracing::info!(account_id, status, "Friendbot response accepted");
        return Ok(());
    }

    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "(unreadable body)".to_string());
    Err(format!(
        "Friendbot returned {status} for {account_id}: {body}"
    ))
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

        let stroops = check_keeper_balance(&cfg).await.unwrap();
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

        let err = check_keeper_balance(&cfg).await.unwrap_err();
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

        let err = check_keeper_balance(&cfg).await.unwrap_err();
        assert!(matches!(err, RpcError::NetworkError(_)));
    }

    // ── fund_keeper_via_friendbot — HTTP-level tests ─────────────────────────

    /// Verifies that a 400 response from Friendbot (account already funded) is
    /// treated as success — the operation is idempotent.
    #[tokio::test]
    async fn fund_keeper_via_friendbot_already_funded_400_returns_ok() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(400).set_body_string(
                    r#"{"detail":"createAccountAlreadyExist","status":400,"title":"Transaction Failed"}"#,
                ),
            )
            .mount(&server)
            .await;

        let result = super::fund_keeper_at(&server.uri(), "GNEWACCOUNT").await;
        assert!(result.is_ok());
    }

    /// Verifies that a 200 response from Friendbot (new account funded) returns Ok.
    #[tokio::test]
    async fn fund_keeper_via_friendbot_new_account_200_returns_ok() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"hash":"abc123","ledger":12345}"#),
            )
            .mount(&server)
            .await;

        let result = super::fund_keeper_at(&server.uri(), "GNEWACCOUNT").await;
        assert!(result.is_ok());
    }
}
