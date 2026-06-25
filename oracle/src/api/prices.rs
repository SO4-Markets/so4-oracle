use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use super::{AdminAuth, ApiError};
use crate::keeper::{build_balance_response, check_keeper_balance, KeeperBalanceConfig};
use crate::state::{AppState, CachedPrice, FailedSubmission};
use crate::stellar_rpc::get_latest_ledger_sequence;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct FailuresResponse {
    pub failures: Vec<FailedSubmission>,
}

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub async fn ready(State(state): State<Arc<AppState>>) -> Result<Json<HealthResponse>, ApiError> {
    get_latest_ledger_sequence(&state.config.stellar_rpc_url)
        .await
        .map_err(|err| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("rpc_unhealthy: {err}"),
            )
        })?;

    let cfg = KeeperBalanceConfig {
        horizon_url: state.config.horizon_url.clone(),
        account_id: state.config.keeper_account_id.clone(),
        min_balance_xlm: state.config.min_keeper_balance_xlm,
    };
    let stroops = check_keeper_balance(&cfg).await.map_err(|err| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("keeper_balance_unavailable: {err}"),
        )
    })?;
    let balance = build_balance_response(&cfg, stroops);

    if balance.below_minimum {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "keeper_balance_below_minimum",
        ));
    }

    Ok(Json(HealthResponse { status: "ok" }))
}

pub async fn prices(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<CachedPrice>>, ApiError> {
    let cache = state.price_cache.read().await;
    if cache.prices.is_empty() {
        return Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "no_prices"));
    }
    Ok(Json(cache.prices.values().cloned().collect()))
}

pub async fn failed_submissions(
    _auth: AdminAuth,
    State(state): State<Arc<AppState>>,
) -> Json<FailuresResponse> {
    let failures = state.failures.lock().await.iter().rev().cloned().collect();

    Json(FailuresResponse { failures })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Network, PriceFeedConfig, SecretString};
    use axum::response::IntoResponse;
    use serde_json::json;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_state(
        stellar_rpc_url: String,
        horizon_url: String,
        min_keeper_balance_xlm: f64,
    ) -> Arc<AppState> {
        let config = Config {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            network: Network::Testnet,
            network_passphrase: "Test SDF Network ; September 2015".to_string(),
            stellar_rpc_url,
            horizon_url,
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
            min_keeper_balance_xlm,
            price_loop_interval: Duration::from_millis(1),
            keeper_loop_interval: Duration::from_millis(1),
            price_feed: PriceFeedConfig { tokens: vec![] },
        };

        Arc::new(AppState::new(Arc::new(config)))
    }

    async fn mount_latest_ledger(rpc: &MockServer, status: u16) {
        let response = if status == 200 {
            ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "id": "abc123",
                    "sequence": 12345,
                    "protocolVersion": 22
                }
            }))
        } else {
            ResponseTemplate::new(status)
        };

        Mock::given(method("POST"))
            .respond_with(response)
            .mount(rpc)
            .await;
    }

    async fn mount_keeper_balance(horizon: &MockServer, balance: &str) {
        Mock::given(method("GET"))
            .and(path("/accounts/GACCOUNT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "GACCOUNT",
                "balances": [
                    {"asset_type": "native", "balance": balance}
                ]
            })))
            .mount(horizon)
            .await;
    }

    #[tokio::test]
    async fn ready_returns_ok_when_rpc_and_keeper_balance_are_healthy() {
        let rpc = MockServer::start().await;
        let horizon = MockServer::start().await;
        mount_latest_ledger(&rpc, 200).await;
        mount_keeper_balance(&horizon, "20.0000000").await;

        let response = ready(State(test_state(rpc.uri(), horizon.uri(), 10.0)))
            .await
            .expect("ready should succeed");

        assert_eq!(response.0.status, "ok");
    }

    #[tokio::test]
    async fn ready_returns_503_when_rpc_is_unreachable() {
        let rpc = MockServer::start().await;
        let horizon = MockServer::start().await;
        mount_latest_ledger(&rpc, 500).await;
        mount_keeper_balance(&horizon, "20.0000000").await;

        let err = ready(State(test_state(rpc.uri(), horizon.uri(), 10.0)))
            .await
            .unwrap_err();

        assert_eq!(
            err.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn ready_returns_503_when_keeper_balance_is_below_minimum() {
        let rpc = MockServer::start().await;
        let horizon = MockServer::start().await;
        mount_latest_ledger(&rpc, 200).await;
        mount_keeper_balance(&horizon, "3.0000000").await;

        let err = ready(State(test_state(rpc.uri(), horizon.uri(), 10.0)))
            .await
            .unwrap_err();

        assert_eq!(
            err.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
