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

/// Load keeper balance config from the worker environment.
///
/// Reads:
/// - `KEEPER_ACCOUNT_ID` (required)
/// - `HORIZON_URL` (optional; defaults based on `network_config::StellarNetwork`)
/// - `MIN_KEEPER_BALANCE_XLM` (optional; defaults to 10.0)
pub fn load_keeper_config(
    default_horizon_url: &str,
) -> Result<KeeperBalanceConfig, String> {
    let account_id = std::env::var("KEEPER_ACCOUNT_ID")
        .map_err(|_| "KEEPER_ACCOUNT_ID is not set".to_string())?;

    let horizon_url = std::env::var("HORIZON_URL")
        .unwrap_or_else(|_| default_horizon_url.to_string());

    let min_balance_xlm = std::env::var("MIN_KEEPER_BALANCE_XLM")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(DEFAULT_MIN_KEEPER_BALANCE_XLM);

    Ok(KeeperBalanceConfig {
        horizon_url,
        account_id,
        min_balance_xlm,
    })
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
            "[keeper] CRITICAL: balance {xlm:.7} XLM is below minimum {:.7} XLM — \
             consider topping up account {}",
            cfg.min_balance_xlm,
            cfg.account_id
        );
    } else {
        tracing::info!(
            "[keeper] balance ok: {xlm:.7} XLM (min={:.7})",
            cfg.min_balance_xlm
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

/// Testnet Friendbot URL base (Issue #120).
pub const FRIENDBOT_URL: &str = "https://friendbot.stellar.org";

/// Call the Stellar testnet Friendbot to fund `account_id`.
///
/// Uses `worker::Fetch` (the only HTTP client available in the WASM Worker
/// runtime).  Returns `Ok(())` on a 200/400 response (400 means the account
/// already exists/is funded, which is not an error).  Returns `Err` on
/// network failures or other non-success statuses.
///
/// This function must be called directly inside an `async` Cloudflare Worker
/// handler — it must **not** be spawned, because `worker::Fetch` futures are
/// `!Send`.
pub async fn fund_keeper_via_friendbot(account_id: &str) -> Result<(), String> {
    let url = format!("{FRIENDBOT_URL}?addr={account_id}");
    tracing::info!("[keeper] calling Friendbot for account {account_id}");
    let response = reqwest::get(&url)
        .await
        .map_err(|e| format!("friendbot request failed: {e}"))?;
    let status = response.status().as_u16();
    if status == 200 || status == 400 {
        tracing::info!("[keeper] Friendbot response {status} for {account_id}");
        return Ok(());
    }
    let body = response.text().await.unwrap_or_else(|_| "(unreadable body)".to_string());
    Err(format!(
        "Friendbot returned {status} for {account_id}: {body}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stellar_rpc::parse_account_balance_response;

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
}
