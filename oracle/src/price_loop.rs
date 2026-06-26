use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use shared_config::TokenConfig;
use tokio::time::{interval, MissedTickBehavior};

use crate::prices::AggregatedPrice;
use crate::state::{AppState, CachedPrice, FailedSubmission};

const SOURCE_RETRY_ATTEMPTS: u32 = 3;
const SOURCE_RETRY_BASE_DELAY_MS: u64 = 100;

pub async fn run_price_loop(state: Arc<AppState>) {
    let mut ticker = interval(state.config.price_loop_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;
        run_price_cycle(Arc::clone(&state)).await;
    }
}

pub async fn run_price_cycle(state: Arc<AppState>) {
    let started = Instant::now();
    {
        let mut status = state.cycle_status.write().await;
        status.price_cycle_running = true;
    }

    let mut tokens_ok = 0usize;
    let mut tokens_failed = 0usize;
    let ledger_seq =
        match crate::stellar_rpc::get_latest_ledger_sequence(&state.config.stellar_rpc_url).await {
            Ok(ledger_seq) => ledger_seq,
            Err(error) => {
                record_error(&state, "get_latest_ledger", error.to_string()).await;
                finish_cycle(&state, started, tokens_ok, tokens_failed).await;
                return;
            }
        };

    for token in &state.config.price_feed.tokens {
        match build_cached_price(&state, token, ledger_seq).await {
            Ok(price) => {
                let key = token.lookup_key();
                state.price_cache.write().await.prices.insert(key, price);
                tokens_ok += 1;
            }
            Err(error) => {
                tokens_failed += 1;
                record_error(&state, format!("price:{}", token.symbol), error).await;
            }
        }
    }

    if tokens_ok > 0 {
        state.price_cache.write().await.last_updated = Some(SystemTime::now());
    }

    finish_cycle(&state, started, tokens_ok, tokens_failed).await;
}

async fn finish_cycle(
    state: &Arc<AppState>,
    started: Instant,
    tokens_ok: usize,
    tokens_failed: usize,
) {
    {
        let mut status = state.cycle_status.write().await;
        status.price_cycle_running = false;
        status.last_price_cycle_at = Some(SystemTime::now());
    }

    let latency_ms = started.elapsed().as_millis() as u64;
    state.metrics.record_price_cycle(latency_ms);

    tracing::info!(tokens_ok, tokens_failed, latency_ms, "cycle_complete");
}

async fn build_cached_price(
    state: &Arc<AppState>,
    token: &TokenConfig,
    ledger_seq: u32,
) -> Result<CachedPrice, String> {
    let mut prices = Vec::new();
    let mut sources = Vec::new();

    for source in &token.sources {
        match fetch_source_with_retry(source, token).await {
            Ok(price) => {
                prices.push(price);
                sources.push(source.clone());
            }
            Err(error) => {
                record_error(
                    state,
                    format!("source:{}:{}", token.symbol, source),
                    error.clone(),
                )
                .await;
                tracing::warn!(symbol = %token.symbol, source = %source, error = %error, "price source failed");
            }
        }
    }

    let aggregate = crate::prices::aggregate_prices(
        &prices,
        &sources,
        token.min_sources,
        token.max_deviation_bps,
    )?;
    signed_cached_price(state, token, ledger_seq, aggregate)
}

async fn fetch_source_with_retry(source: &str, token: &TokenConfig) -> Result<i128, String> {
    crate::retry::retry_with_backoff(
        || async { fetch_source_price(source, token).await },
        SOURCE_RETRY_ATTEMPTS,
        SOURCE_RETRY_BASE_DELAY_MS,
    )
    .await
}

async fn fetch_source_price(source: &str, token: &TokenConfig) -> Result<i128, String> {
    match source {
        "binance" => {
            let symbol = token
                .binance_symbol
                .as_ref()
                .ok_or_else(|| "missing binance_symbol".to_string())?;
            let results = crate::binance::fetch_spot_prices(std::slice::from_ref(symbol))
                .await
                .map_err(|err| format!("{err:?}"))?;
            results
                .into_iter()
                .find(|(got_symbol, _)| got_symbol == symbol)
                .map(|(_, price)| price)
                .ok_or_else(|| format!("binance symbol not returned: {symbol}"))
        }
        "coinbase" => {
            let symbol = token
                .coinbase_symbol
                .as_ref()
                .ok_or_else(|| "missing coinbase_symbol".to_string())?;
            crate::coinbase::fetch_spot_price(symbol)
                .await
                .map_err(|err| format!("{err:?}"))
        }
        "pyth" => {
            let feed_id = token
                .pyth_feed_id
                .as_ref()
                .ok_or_else(|| "missing pyth_feed_id".to_string())?;
            crate::pyth::fetch_pyth_price(feed_id, token.stale_after_seconds, 50)
                .await
                .map_err(|err| format!("{err:?}"))
        }
        "fixed" => crate::fixed::fixed_price(token).map_err(|err| format!("{err:?}")),
        other => Err(format!("unsupported source: {other}")),
    }
}

fn signed_cached_price(
    state: &Arc<AppState>,
    token: &TokenConfig,
    ledger_seq: u32,
    aggregate: AggregatedPrice,
) -> Result<CachedPrice, String> {
    let timestamp = current_timestamp_secs();
    let signature = crate::signing::sign_price(
        state.config.keeper_private_key.as_str(),
        &state.config.network_passphrase,
        ledger_seq,
        &token.stellar_address,
        aggregate.min,
        aggregate.max,
        timestamp,
    )
    .map_err(|err| err.to_string())?;

    Ok(CachedPrice {
        token_address: token.stellar_address.clone(),
        symbol: token.symbol.clone(),
        display_symbol: token.display_symbol().to_string(),
        min: aggregate.min,
        max: aggregate.max,
        median: aggregate.median,
        timestamp,
        ledger_seq,
        sources_used: aggregate.sources_used,
        signature: hex::encode(signature.to_bytes()),
    })
}

async fn record_error(
    state: &Arc<AppState>,
    operation: impl Into<String>,
    error: impl Into<String>,
) {
    state.failures.lock().await.push(FailedSubmission {
        at: SystemTime::now(),
        operation: operation.into(),
        network: state.config.network.as_str().to_string(),
        token: String::new(),
        symbol: String::new(),
        min: 0,
        max: 0,
        tx_hash: None,
        error: error.into(),
        timestamp: 0,
        ledger_seq: 0,
    });
}

fn current_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Network, PriceFeedConfig, SecretString};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    fn test_state(token: TokenConfig) -> Arc<AppState> {
        let config = Config {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            network: Network::Testnet,
            network_passphrase: "Test SDF Network ; September 2015".to_string(),
            stellar_rpc_url: "http://localhost:0".to_string(),
            horizon_url: "http://localhost:0".to_string(),
            oracle_contract_id: "CORACLE".to_string(),
            role_store_contract_id: "CROLE".to_string(),
            data_store_contract_id: "CDATA".to_string(),
            order_handler_contract_id: "CORDER".to_string(),
            deposit_handler_contract_id: "CDEPOSIT".to_string(),
            withdrawal_handler_contract_id: "CWITHDRAW".to_string(),
            reader_contract_id: "CREADER".to_string(),
            keeper_private_key: SecretString::new(
                "1111111111111111111111111111111111111111111111111111111111111111".to_string(),
            ),
            keeper_secret_key: SecretString::new("SSECRET".to_string()),
            keeper_account_id: "GACCOUNT".to_string(),
            keeper_index: 0,
            admin_api_token: None,
            min_keeper_balance_xlm: 0.0,
            price_loop_interval: Duration::from_millis(1),
            keeper_loop_interval: Duration::from_millis(1),
            price_feed: PriceFeedConfig {
                tokens: vec![token],
            },
        };
        Arc::new(AppState::new(Arc::new(config)))
    }

    #[tokio::test]
    async fn fixed_source_builds_signed_cached_price() {
        let token = TokenConfig {
            symbol: "TUSDC".to_string(),
            display_symbol: Some("USDC".to_string()),
            stellar_address: "CBAN5YU3KRDKPTQ2H76D6S7HQFPRBGUD524F65BUM2RQCITPTRLKWKES".to_string(),
            sources: vec!["fixed".to_string()],
            binance_symbol: None,
            coinbase_symbol: None,
            pyth_feed_id: None,
            fixed_price: Some("1000000000000000000000000000000".to_string()),
            min_sources: 1,
            max_deviation_bps: 100,
            stale_after_seconds: 60,
            submit_threshold_bps: 10,
            min: 0.0,
            max: 0.0,
            sources_used: vec![],
        };

        let state = test_state(token.clone());
        let cached = build_cached_price(&state, &token, 123).await.unwrap();

        assert_eq!(cached.symbol, "TUSDC");
        assert_eq!(cached.display_symbol, "USDC");
        assert_eq!(cached.ledger_seq, 123);
        assert_eq!(cached.sources_used, vec!["fixed"]);
        assert_eq!(cached.median, 1_000_000_000_000_000_000_000_000_000_000);
        assert_eq!(cached.signature.len(), 128);
    }
}
