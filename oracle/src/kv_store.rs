use serde::{Deserialize, Serialize};
use worker::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedSubmission {
    pub timestamp: u64,
    pub min_price: i128,
    pub max_price: i128,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleStatus {
    pub last_submission_time: Option<u64>,
    pub keeper_balance_xlm: Option<f64>,
    pub tokens: Vec<TokenPrice>,
    pub recent_errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenPrice {
    pub symbol: String,
    pub price: i128,
    pub min: i128,
    pub max: i128,
    pub timestamp: u64,
}

const FAILED_SUBMISSIONS_KV_KEY: &str = "oracle:failed-submissions";
const ORACLE_STATUS_KV_KEY: &str = "oracle:status";
const FAILED_SUBMISSION_TTL_SECONDS: u32 = 600;
const LAST_SUBMITTED_PRICE_KV_PREFIX: &str = "oracle:last-price:";
const CACHED_PRICES_KV_KEY: &str = "oracle:cached-prices";

pub async fn store_failed_submission(
    env: &Env,
    min_price: i128,
    max_price: i128,
    error: &str,
) -> Result<(), String> {
    let kv = env
        .kv("ORACLE_KV")
        .map_err(|e| format!("failed to get KV namespace: {}", e))?;

    let timestamp = current_timestamp();
    let submission = FailedSubmission {
        timestamp,
        min_price,
        max_price,
        error: error.to_string(),
    };

    let value = serde_json::to_string(&submission)
        .map_err(|e| format!("failed to serialize submission: {}", e))?;

    let key = format!("{}:{}", FAILED_SUBMISSIONS_KV_KEY, timestamp);
    kv.put(&key, &value)
        .map_err(|e| format!("failed to put in KV: {}", e))?
        .expiration(FAILED_SUBMISSION_TTL_SECONDS as u64)
        .execute()
        .await
        .map_err(|e| format!("failed to store submission: {}", e))?;

    console_log!("[oracle] stored failed submission in KV: {key}");
    Ok(())
}

pub async fn get_failed_submissions(env: &Env) -> Result<Vec<FailedSubmission>, String> {
    let kv = env
        .kv("ORACLE_KV")
        .map_err(|e| format!("failed to get KV namespace: {}", e))?;

    let keys = kv
        .list()
        .prefix(FAILED_SUBMISSIONS_KV_KEY.to_string())
        .execute()
        .await
        .map_err(|e| format!("failed to list failed submissions: {}", e))?;

    let mut submissions = Vec::new();
    for key_metadata in keys.keys {
        if let Ok(Some(value)) = kv.get(&key_metadata.name).text().await {
            if let Ok(submission) = serde_json::from_str::<FailedSubmission>(&value) {
                submissions.push(submission);
            }
        }
    }

    submissions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(submissions)
}

pub async fn store_oracle_status(env: &Env, status: &OracleStatus) -> Result<(), String> {
    let kv = env
        .kv("ORACLE_KV")
        .map_err(|e| format!("failed to get KV namespace: {}", e))?;

    let value =
        serde_json::to_string(status).map_err(|e| format!("failed to serialize status: {}", e))?;

    kv.put(ORACLE_STATUS_KV_KEY, &value)
        .map_err(|e| format!("failed to put in KV: {}", e))?
        .execute()
        .await
        .map_err(|e| format!("failed to store status: {}", e))?;

    console_log!("[oracle] stored oracle status in KV");
    Ok(())
}

pub async fn get_oracle_status(env: &Env) -> Result<OracleStatus, String> {
    let kv = env
        .kv("ORACLE_KV")
        .map_err(|e| format!("failed to get KV namespace: {}", e))?;

    match kv.get(ORACLE_STATUS_KV_KEY).text().await {
        Ok(Some(value)) => serde_json::from_str::<OracleStatus>(&value)
            .map_err(|e| format!("failed to parse status: {}", e)),
        Ok(None) => Ok(OracleStatus {
            last_submission_time: None,
            keeper_balance_xlm: None,
            tokens: vec![],
            recent_errors: vec![],
        }),
        Err(e) => Err(format!("failed to get status: {}", e)),
    }
}

pub async fn store_last_submitted_price(
    env: &Env,
    token_symbol: &str,
    price: i128,
) -> Result<(), String> {
    let kv = env
        .kv("ORACLE_KV")
        .map_err(|e| format!("failed to get KV namespace: {}", e))?;

    let key = format!("{}{}", LAST_SUBMITTED_PRICE_KV_PREFIX, token_symbol);
    let value = price.to_string();

    kv.put(&key, &value)
        .map_err(|e| format!("failed to put in KV: {}", e))?
        .execute()
        .await
        .map_err(|e| format!("failed to store last price: {}", e))?;

    Ok(())
}

pub async fn get_last_submitted_price(
    env: &Env,
    token_symbol: &str,
) -> Result<Option<i128>, String> {
    let kv = env
        .kv("ORACLE_KV")
        .map_err(|e| format!("failed to get KV namespace: {}", e))?;

    let key = format!("{}{}", LAST_SUBMITTED_PRICE_KV_PREFIX, token_symbol);
    match kv.get(&key).text().await {
        Ok(Some(value)) => {
            let price = value
                .parse::<i128>()
                .map_err(|e| format!("failed to parse price: {}", e))?;
            Ok(Some(price))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(format!("failed to get last price: {}", e)),
    }
}

pub async fn store_cached_prices(env: &Env, prices: &[crate::CachedPrice]) -> Result<(), String> {
    let kv = env
        .kv("ORACLE_KV")
        .map_err(|e| format!("failed to get KV namespace: {}", e))?;

    let value =
        serde_json::to_string(prices).map_err(|e| format!("failed to serialize prices: {}", e))?;

    kv.put(CACHED_PRICES_KV_KEY, &value)
        .map_err(|e| format!("failed to put in KV: {}", e))?
        .execute()
        .await
        .map_err(|e| format!("failed to store prices: {}", e))?;

    Ok(())
}

pub async fn get_cached_prices(env: &Env) -> Result<Vec<crate::CachedPrice>, String> {
    let kv = env
        .kv("ORACLE_KV")
        .map_err(|e| format!("failed to get KV namespace: {}", e))?;

    match kv.get(CACHED_PRICES_KV_KEY).text().await {
        Ok(Some(value)) => serde_json::from_str::<Vec<crate::CachedPrice>>(&value)
            .map_err(|e| format!("failed to parse cached prices: {}", e)),
        Ok(None) => Err("no cached prices".to_string()),
        Err(e) => Err(format!("failed to get cached prices: {}", e)),
    }
}

#[cfg(target_arch = "wasm32")]
fn current_timestamp() -> u64 {
    (js_sys::Date::now() / 1000.0) as u64
}

#[cfg(not(target_arch = "wasm32"))]
fn current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Convenience wrapper: logs a warning on failure instead of propagating.
/// Use for non-critical KV writes (e.g. caching) where the pipeline should
/// continue even if the write fails.
pub async fn set_or_log(env: &Env, key: &str, value: &str) {
    if let Err(e) = set_kv(env, key, value).await {
        worker::console_log!("[oracle] KV write failed for key '{key}': {e}");
    }
}

/// Low-level KV put — returns `Result` so callers can choose to propagate.
async fn set_kv(env: &Env, key: &str, value: &str) -> Result<(), String> {
    let kv = env
        .kv("ORACLE_KV")
        .map_err(|e| format!("failed to get KV namespace: {}", e))?;

    kv.put(key, value)
        .map_err(|e| format!("failed to put in KV: {}", e))?
        .execute()
        .await
        .map_err(|e| format!("failed to execute KV put: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_failed_submission_serialization() {
        let submission = FailedSubmission {
            timestamp: 1234567890,
            min_price: 45000,
            max_price: 46000,
            error: "Network timeout".to_string(),
        };

        let json = serde_json::to_string(&submission).unwrap();
        let deserialized: FailedSubmission = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.timestamp, 1234567890);
        assert_eq!(deserialized.min_price, 45000);
        assert_eq!(deserialized.max_price, 46000);
        assert_eq!(deserialized.error, "Network timeout");
    }

    #[test]
    fn test_oracle_status_serialization() {
        let status = OracleStatus {
            last_submission_time: Some(1234567890),
            keeper_balance_xlm: Some(100.5),
            tokens: vec![TokenPrice {
                symbol: "BTC".to_string(),
                price: 45000,
                min: 44900,
                max: 45100,
                timestamp: 1234567890,
            }],
            recent_errors: vec!["Network error".to_string()],
        };

        let json = serde_json::to_string(&status).unwrap();
        let deserialized: OracleStatus = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.last_submission_time, Some(1234567890));
        assert_eq!(deserialized.keeper_balance_xlm, Some(100.5));
        assert_eq!(deserialized.tokens.len(), 1);
        assert_eq!(deserialized.tokens[0].symbol, "BTC");
        assert_eq!(deserialized.recent_errors.len(), 1);
    }

    #[test]
    fn test_token_price_creation() {
        let token = TokenPrice {
            symbol: "ETH".to_string(),
            price: 2500,
            min: 2400,
            max: 2600,
            timestamp: 1234567890,
        };

        assert_eq!(token.symbol, "ETH");
        assert_eq!(token.price, 2500);
        assert!(token.min < token.price);
        assert!(token.max > token.price);
    }

    #[test]
    fn test_oracle_status_default() {
        let status = OracleStatus {
            last_submission_time: None,
            keeper_balance_xlm: None,
            tokens: vec![],
            recent_errors: vec![],
        };

        assert!(status.last_submission_time.is_none());
        assert!(status.keeper_balance_xlm.is_none());
        assert!(status.tokens.is_empty());
        assert!(status.recent_errors.is_empty());
    }
}
