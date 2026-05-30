use axum::{routing::get, Router};
use tower_service::Service;
use worker::*;

pub mod binance;
pub mod coinbase;
pub mod config;
pub mod keeper;
pub mod kv_store;
pub mod log;
pub mod network_config;
pub mod prices;
pub mod pyth;
pub mod retry;
pub mod signing;
pub mod stellar_rpc;
pub mod submit;

use network_config::StellarNetwork;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedPrice {
    pub token: String,
    pub symbol: String,
    pub min: i128,
    pub max: i128,
    pub timestamp: u64,
    pub sources_used: Vec<String>,
}

fn router() -> Router {
    Router::new().route("/", get(root))
}

/// HTTP fetch handler.
///
/// Most routes are handled by Axum.  The `/keeper/balance` route is handled
/// directly here because it makes async `worker::Fetch` calls, whose futures
/// are not `Send`, preventing them from satisfying Axum's `Handler` bound on
/// this WASM target.
#[event(fetch)]
async fn fetch(
    req: HttpRequest,
    env: Env,
    _ctx: Context,
) -> Result<axum::http::Response<axum::body::Body>> {
    let path = req.uri().path().to_string();
    match path.as_str() {
        "/keeper/balance" => handle_keeper_balance(&env).await,
        "/oracle/status" => handle_oracle_status(&env).await,
        "/oracle/failed-submissions" => handle_failed_submissions(&env).await,
        "/prices" => handle_get_prices(&env).await,
        _ => Ok(router().call(req).await?),
    }
}

/// `GET /keeper/balance` — current XLM balance of the keeper account.
async fn handle_keeper_balance(env: &Env) -> Result<axum::http::Response<axum::body::Body>> {
    let net_cfg = match network_config::load_network_config(env) {
        Ok(c) => c,
        Err(e) => return json_error(503, &e.to_string()),
    };
    let horizon_url = default_horizon_url(&net_cfg.network);
    let keeper_cfg = match keeper::load_keeper_config(env, horizon_url) {
        Ok(c) => c,
        Err(e) => return json_error(503, &e),
    };
    match keeper::check_keeper_balance(&keeper_cfg).await {
        Ok(stroops) => {
            let resp = keeper::build_balance_response(&keeper_cfg, stroops);
            let body = serde_json::to_string(&resp)
                .unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string());
            Ok(axum::http::Response::builder()
                .status(200)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(body))
                .unwrap())
        }
        Err(e) => json_error(503, &e.to_string()),
    }
}

fn json_error(status: u16, msg: &str) -> Result<axum::http::Response<axum::body::Body>> {
    let body = format!(r#"{{"error":{msg:?}}}"#);
    Ok(axum::http::Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(body))
        .unwrap())
}

/// Scheduled handler — runs the full price-update pipeline on every cron tick.
///
/// Local testing: `wrangler dev --test-scheduled`
#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) -> Result<()> {
    use serde_json::json;

    let start_time = current_timestamp();

    // 1. Parse feed configuration.
    let feed_cfg = match config::load_from_env(&env) {
        Ok(cfg) => cfg,
        Err(e) => {
            log::error("config_error", json!({"error": e.to_string()}));
            return Err(Error::from(e.to_string()));
        }
    };

    // 2. Load network config.
    let net_cfg = match network_config::load_network_config(&env) {
        Ok(cfg) => cfg,
        Err(e) => {
            log::error("network_config_error", json!({"error": e.to_string()}));
            return Err(Error::from(e.to_string()));
        }
    };

    log::info(
        "cycle_start",
        json!({"network": format!("{:?}", net_cfg.network)}),
    );

    // 3. Check keeper balance.
    let horizon_url = default_horizon_url(&net_cfg.network);
    let keeper_cfg = match keeper::load_keeper_config(&env, horizon_url) {
        Ok(cfg) => cfg,
        Err(e) => {
            log::error("keeper_config_error", json!({"error": e.to_string()}));
            return Err(Error::from(e));
        }
    };

    let balance_stroops = match keeper::check_keeper_balance(&keeper_cfg).await {
        Ok(b) => b,
        Err(e) => {
            log::error("balance_check_error", json!({"error": e.to_string()}));
            return Err(Error::from(e.to_string()));
        }
    };

    let balance_xlm = balance_stroops as f64 / keeper::XLM_IN_STROOPS as f64;
    if balance_xlm < keeper_cfg.min_balance_xlm {
        // Issue #120 — auto-fund on testnet; alert-only on mainnet.
        match net_cfg.network {
            StellarNetwork::Testnet => {
                log::warn(
                    "low_balance_funding",
                    json!({
                        "balance_xlm": balance_xlm,
                        "min_balance_xlm": keeper_cfg.min_balance_xlm,
                        "action": "calling_friendbot"
                    }),
                );
                match keeper::fund_keeper_via_friendbot(&keeper_cfg.account_id).await {
                    Ok(()) => log::info(
                        "friendbot_funded",
                        json!({"account": keeper_cfg.account_id}),
                    ),
                    Err(e) => log::error("friendbot_failed", json!({"error": e})),
                }
                // Skip this cycle — balance will be confirmed on next cron tick.
                return Ok(());
            }
            StellarNetwork::Mainnet => {
                log::error(
                    "insufficient_balance",
                    json!({
                        "balance_xlm": balance_xlm,
                        "min_balance_xlm": keeper_cfg.min_balance_xlm,
                        "action": "manual_top_up_required"
                    }),
                );
                return Ok(());
            }
        }
    }

    // 4. Fetch ledger sequence.
    let ledger_seq = match stellar_rpc::get_latest_ledger_sequence(&net_cfg.rpc_url).await {
        Ok(seq) => seq,
        Err(e) => {
            log::error("ledger_fetch_error", json!({"error": e.to_string()}));
            return Err(Error::from(e.to_string()));
        }
    };

    // 5. Fetch prices from all sources.
    #[derive(Debug)]
    struct TokenPrices {
        prices: Vec<i128>,
        sources: Vec<String>,
    }

    let mut all_prices: std::collections::BTreeMap<String, TokenPrices> =
        std::collections::BTreeMap::new();

    for token in &feed_cfg.tokens {
        let mut token_prices = Vec::new();
        let mut sources_used = Vec::new();

        for source in &token.sources {
            match source.as_str() {
                "binance" => {
                    let symbol = token
                        .binance_symbol
                        .as_ref()
                        .map(|s| s.clone())
                        .unwrap_or_else(|| format!("{}USDT", token.symbol));

                    match retry::retry_with_backoff(
                        || {
                            let sym = symbol.clone();
                            async move { binance::fetch_spot_prices(&[sym]).await }
                        },
                        3,
                        200,
                    )
                    .await
                    {
                        Ok(prices) => {
                            if !prices.is_empty() {
                                token_prices.push(prices[0].1);
                                sources_used.push("binance".to_string());
                            }
                        }
                        Err(e) => {
                            log::error(
                                "binance_fetch_error",
                                json!({"token": token.symbol.clone(), "error": format!("{:?}", e)}),
                            );
                        }
                    }
                }
                "pyth" => {
                    if let Some(feed_id) = &token.pyth_feed_id {
                        match pyth::fetch_pyth_price(feed_id).await {
                            Ok(price) => {
                                token_prices.push(price);
                                sources_used.push("pyth".to_string());
                            }
                            Err(e) => {
                                log::error(
                                    "pyth_fetch_error",
                                    json!({"token": token.symbol.clone(), "error": format!("{:?}", e)}),
                                );
                            }
                        }
                    }
                }
                "coinbase" => {
                    match retry::retry_with_backoff(
                        || {
                            let sym = token.symbol.clone();
                            async move { coinbase::fetch_spot_price(&sym).await }
                        },
                        3,
                        200,
                    )
                    .await
                    {
                        Ok(price) => {
                            token_prices.push(price);
                            sources_used.push("coinbase".to_string());
                        }
                        Err(e) => {
                            log::error(
                                "coinbase_fetch_error",
                                json!({"token": token.symbol.clone(), "error": format!("{:?}", e)}),
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        if !token_prices.is_empty() {
            all_prices.insert(
                token.symbol.clone(),
                TokenPrices {
                    prices: token_prices,
                    sources: sources_used,
                },
            );
        }
    }

    if all_prices.is_empty() {
        log::error("no_prices_fetched", json!({}));
        return Ok(());
    }

    // 6. Apply circuit breaker checks.
    let threshold_percent: f64 = env
        .var("PRICE_MOVEMENT_THRESHOLD")
        .ok()
        .and_then(|v| v.to_string().parse().ok())
        .unwrap_or(10.0);

    let mut cached_prices: Vec<CachedPrice> = Vec::new();

    for token in &feed_cfg.tokens {
        if let Some(token_prices) = all_prices.get(&token.symbol) {
            if token_prices.prices.is_empty() {
                continue;
            }

            if token_prices.prices.len() < 2 {
                log::warn(
                    "insufficient_sources",
                    json!({"token": token.symbol.clone(), "sources_count": token_prices.prices.len()}),
                );
                continue;
            }

            // 6a. Filter outliers > 3σ from median
            let filter_result =
                prices::filter_outliers(&token_prices.prices, &token_prices.sources);

            for (src, price, dev) in &filter_result.rejected {
                log::error(
                    "outlier_rejected",
                    json!({"token": token.symbol.clone(), "source": src, "price": price, "deviation": dev}),
                );
            }

            if filter_result.filtered_prices.is_empty() {
                log::error(
                    "all_sources_rejected_as_outliers",
                    json!({"token": token.symbol.clone()}),
                );
                continue;
            }

            let aggregated = prices::compute_confidence_interval(&filter_result.filtered_prices);
            let (min, max) = match aggregated {
                Some(props) => (props.min, props.max),
                None => continue,
            };

            let median = prices::compute_median(&filter_result.filtered_prices).unwrap();

            let last_price = kv_store::get_last_submitted_price(&env, &token.symbol)
                .await
                .ok()
                .flatten();

            let blocked = if let Some(last) = last_price {
                let percent_change = ((median as f64 - last as f64) / last as f64).abs() * 100.0;
                if percent_change > threshold_percent {
                    log::error(
                        "price_movement_exceeded",
                        json!({"token": token.symbol.clone(), "old_price": last, "new_price": median, "percent_change": percent_change}),
                    );
                    true
                } else {
                    false
                }
            } else {
                false
            };

            if !blocked {
                if let Err(e) = kv_store::store_last_submitted_price(&env, &token.symbol, median).await {
                    log::warn(
                        "kv_write_failed",
                        serde_json::json!({"key": "last_price", "token": token.symbol, "error": e}),
                    );
                }
                let timestamp = current_timestamp_secs();
                cached_prices.push(CachedPrice {
                    token: token.stellar_address.clone(),
                    symbol: token.symbol.clone(),
                    min,
                    max,
                    timestamp,
                    sources_used: filter_result.filtered_sources.clone(),
                });
            }
        }
    }

    // 7. Cache the prices.
    if !cached_prices.is_empty() {
        if let Err(e) = kv_store::store_cached_prices(&env, &cached_prices).await {
            log::warn(
                "kv_write_failed",
                serde_json::json!({"key": "cached_prices", "error": e}),
            );
        }
        log::info(
            "prices_cached",
            json!({"count": cached_prices.len(), "timestamp": current_timestamp_secs()}),
        );
    }

    let latency = current_timestamp() - start_time;
    log::info(
        "cycle_complete",
        json!({"ledger_seq": ledger_seq, "prices_cached": cached_prices.len(), "latency_ms": latency}),
    );

    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn current_timestamp() -> u64 {
    js_sys::Date::now() as u64
}

#[cfg(not(target_arch = "wasm32"))]
fn current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(target_arch = "wasm32")]
fn current_timestamp_secs() -> u64 {
    (js_sys::Date::now() / 1000.0) as u64
}

#[cfg(not(target_arch = "wasm32"))]
fn current_timestamp_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn handle_oracle_status(env: &Env) -> Result<axum::http::Response<axum::body::Body>> {
    match kv_store::get_oracle_status(env).await {
        Ok(status) => {
            let body = serde_json::to_string(&status)
                .unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string());
            Ok(axum::http::Response::builder()
                .status(200)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(body))
                .unwrap())
        }
        Err(e) => json_error(503, &e),
    }
}

async fn handle_failed_submissions(env: &Env) -> Result<axum::http::Response<axum::body::Body>> {
    match kv_store::get_failed_submissions(env).await {
        Ok(submissions) => {
            let body = serde_json::to_string(&submissions)
                .unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string());
            Ok(axum::http::Response::builder()
                .status(200)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(body))
                .unwrap())
        }
        Err(e) => json_error(503, &e),
    }
}

pub async fn root() -> &'static str {
    "Hello Axum!"
}

async fn handle_get_prices(env: &Env) -> Result<axum::http::Response<axum::body::Body>> {
    match kv_store::get_cached_prices(env).await {
        Ok(prices) => {
            if prices.is_empty() {
                let body = r#"{"error":"no_prices","reason":"cache_empty"}"#;
                return Ok(axum::http::Response::builder()
                    .status(503)
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(body))
                    .unwrap());
            }
            let body = serde_json::to_string(&prices)
                .unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string());
            Ok(axum::http::Response::builder()
                .status(200)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(body))
                .unwrap())
        }
        Err(_) => {
            let body = r#"{"error":"no_prices","reason":"cache_empty"}"#;
            Ok(axum::http::Response::builder()
                .status(503)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(body))
                .unwrap())
        }
    }
}

fn default_horizon_url(network: &StellarNetwork) -> &'static str {
    match network {
        StellarNetwork::Testnet => "https://horizon-testnet.stellar.org",
        StellarNetwork::Mainnet => "https://horizon.stellar.org",
    }
}
