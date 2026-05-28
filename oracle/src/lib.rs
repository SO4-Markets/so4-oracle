use axum::{routing::get, Router};
use tower_service::Service;
use worker::*;

pub mod binance;
pub mod config;
pub mod keeper;
pub mod network_config;
pub mod prices;
pub mod retry;
pub mod stellar_rpc;
pub mod submit;

use network_config::StellarNetwork;

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
    if path == "/keeper/balance" {
        return handle_keeper_balance(&env).await;
    }
    Ok(router().call(req).await?)
}

/// `GET /keeper/balance` — current XLM balance of the keeper account.
async fn handle_keeper_balance(
    env: &Env,
) -> Result<axum::http::Response<axum::body::Body>> {
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
    // 1. Parse feed configuration.
    let feed_cfg = match config::load_from_env(&env) {
        Ok(cfg) => cfg,
        Err(e) => {
            console_error!("[oracle] startup config error: {e}");
            return Err(Error::from(e.to_string()));
        }
    };

    // 2. Load network config (STELLAR_NETWORK selects testnet/mainnet defaults).
    let net_cfg = match network_config::load_network_config(&env) {
        Ok(cfg) => cfg,
        Err(e) => {
            console_error!("[oracle] network config error: {e}");
            return Err(Error::from(e.to_string()));
        }
    };
    console_log!(
        "[oracle] network={:?} rpc={}",
        net_cfg.network,
        net_cfg.rpc_url
    );

    // 3. Check keeper balance before doing anything on-chain.
    let horizon_url = default_horizon_url(&net_cfg.network);
    let keeper_cfg = match keeper::load_keeper_config(&env, horizon_url) {
        Ok(cfg) => cfg,
        Err(e) => {
            console_error!("[oracle] keeper config error: {e}");
            return Err(Error::from(e));
        }
    };

    let balance_stroops = match keeper::check_keeper_balance(&keeper_cfg).await {
        Ok(b) => b,
        Err(e) => {
            console_error!("[oracle] balance check failed: {e}");
            return Err(Error::from(e.to_string()));
        }
    };

    let balance_xlm = balance_stroops as f64 / keeper::XLM_IN_STROOPS as f64;
    if balance_xlm < keeper_cfg.min_balance_xlm {
        console_error!(
            "[oracle] skipping submission — balance {balance_xlm:.7} XLM below minimum {:.7}",
            keeper_cfg.min_balance_xlm
        );
        return Ok(());
    }

    // 4. Fetch the current ledger sequence.
    let ledger_seq = match stellar_rpc::get_latest_ledger_sequence(&net_cfg.rpc_url).await {
        Ok(seq) => {
            console_log!("[oracle] ledger sequence: {seq}");
            seq
        }
        Err(e) => {
            console_error!("[oracle] failed to fetch ledger sequence: {e}");
            return Err(Error::from(e.to_string()));
        }
    };

    // 5. Fetch prices with retry (exponential backoff, 3 attempts, 200 ms base).
    let binance_symbols: Vec<String> = feed_cfg
        .tokens
        .iter()
        .filter(|t| t.sources.iter().any(|s| s == "binance"))
        .map(|t| format!("{}USDT", t.symbol))
        .collect();

    let raw_prices = if !binance_symbols.is_empty() {
        let symbols = binance_symbols.clone();
        match retry::retry_with_backoff(
            || {
                let syms = symbols.clone();
                async move { binance::fetch_spot_prices(&syms).await }
            },
            3,
            200,
        )
        .await
        {
            Ok(p) => {
                console_log!("[oracle] fetched {} price(s) from Binance", p.len());
                p
            }
            Err(e) => {
                console_error!("[oracle] Binance fetch failed after retries: {e:?}");
                return Err(Error::from("price fetch failed"));
            }
        }
    } else {
        vec![]
    };

    if raw_prices.is_empty() {
        console_log!("[oracle] no prices to submit at ledger {ledger_seq}");
        return Ok(());
    }

    // 6. Compute confidence interval (10th/90th percentile spread).
    let price_values: Vec<i128> = raw_prices.iter().map(|(_, p)| *p).collect();
    let spread = prices::compute_confidence_interval(&price_values);
    console_log!(
        "[oracle] price spread: {:?} at ledger {ledger_seq}",
        spread
    );

    // 7. TODO: sign PriceProps {min, max} with KEEPER_SECRET_KEY, build and
    //    submit the Soroban set_prices transaction XDR:
    //
    //    let signed_xdr = build_signed_xdr(&net_cfg, &spread, ledger_seq)?;
    //    let ledger_confirmed = submit::submit_and_poll(&net_cfg.rpc_url, &signed_xdr).await?;
    //    console_log!("[oracle] prices committed at ledger {ledger_confirmed}");

    console_log!(
        "[oracle] scheduled cycle complete — network={:?} ledger_seq={ledger_seq} \
         prices={:?} spread={:?}",
        net_cfg.network,
        raw_prices,
        spread,
    );
    Ok(())
}

pub async fn root() -> &'static str {
    "Hello Axum!"
}

fn default_horizon_url(network: &StellarNetwork) -> &'static str {
    match network {
        StellarNetwork::Testnet => "https://horizon-testnet.stellar.org",
        StellarNetwork::Mainnet => "https://horizon.stellar.org",
    }
}
