use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Clone)]
pub enum RpcError {
    NetworkError(String),
    HttpError(u16),
    JsonError(String),
    RpcFault { code: i64, message: String },
    BalanceBelowMinimum { balance_xlm: f64, min_xlm: f64 },
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::NetworkError(msg) => write!(f, "network error: {msg}"),
            RpcError::HttpError(code) => write!(f, "HTTP {code}"),
            RpcError::JsonError(msg) => write!(f, "JSON parse error: {msg}"),
            RpcError::RpcFault { code, message } => {
                write!(f, "RPC fault {code}: {message}")
            }
            RpcError::BalanceBelowMinimum { balance_xlm, min_xlm } => {
                write!(f, "balance {balance_xlm} XLM is below minimum {min_xlm} XLM")
            }
        }
    }
}

// ── JSON-RPC wire types ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'a str,
    id: u32,
    method: &'a str,
    params: serde_json::Value,
}

#[derive(Deserialize)]
struct JsonRpcResponse<T> {
    result: Option<T>,
    error: Option<JsonRpcFault>,
}

#[derive(Deserialize)]
struct JsonRpcFault {
    code: i64,
    message: String,
}

// ── getLatestLedger ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct GetLatestLedgerResult {
    sequence: u32,
    #[allow(dead_code)]
    id: String,
    #[serde(rename = "protocolVersion")]
    #[allow(dead_code)]
    protocol_version: serde_json::Value,
}

/// Parse the raw JSON body returned by a `getLatestLedger` RPC call.
///
/// Kept separate from the HTTP layer so it can be unit-tested without
/// mocking the network.
pub fn parse_latest_ledger_response(body: &str) -> Result<u32, RpcError> {
    let resp: JsonRpcResponse<GetLatestLedgerResult> =
        serde_json::from_str(body).map_err(|e| RpcError::JsonError(e.to_string()))?;

    if let Some(fault) = resp.error {
        return Err(RpcError::RpcFault {
            code: fault.code,
            message: fault.message,
        });
    }

    resp.result
        .ok_or_else(|| RpcError::JsonError("missing 'result' field".to_string()))
        .map(|r| r.sequence)
}

/// Call `getLatestLedger` on the Stellar RPC endpoint and return the current
/// ledger sequence number.
///
/// **Caching note:** call this once per price-update cycle and pass the
/// returned value to any downstream function that needs `ledger_seq`.  This
/// avoids redundant round-trips within a single scheduled invocation.
pub async fn get_latest_ledger_sequence(rpc_url: &str) -> Result<u32, RpcError> {
    let payload = serde_json::to_string(&JsonRpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getLatestLedger",
        params: serde_json::Value::Object(serde_json::Map::new()),
    })
    .map_err(|e| RpcError::JsonError(e.to_string()))?;

    let body = rpc_post(rpc_url, payload).await?;
    parse_latest_ledger_response(&body)
}

/// Low-level helper: POST a JSON string to the RPC URL, return the response body.
pub(crate) async fn rpc_post(rpc_url: &str, payload: String) -> Result<String, RpcError> {
    let response = crate::http::client()
        .post(rpc_url)
        .header("Content-Type", "application/json")
        .body(payload)
        .send()
        .await
        .map_err(|e| RpcError::NetworkError(e.to_string()))?;

    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|e| RpcError::NetworkError(e.to_string()))?;

    if status != 200 {
        return Err(RpcError::HttpError(status));
    }

    Ok(body)
}

// ── Account balance (Horizon REST) ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct HorizonBalanceEntry {
    asset_type: String,
    balance: String,
}

#[derive(Debug, Deserialize)]
struct HorizonAccountResponse {
    balances: Vec<HorizonBalanceEntry>,
}

/// Parse the JSON body returned by `GET /accounts/{id}` on a Horizon server.
///
/// Returns the native (XLM) balance in stroops (1 XLM = 10_000_000 stroops).
pub fn parse_account_balance_response(body: &str) -> Result<i64, RpcError> {
    let resp: HorizonAccountResponse =
        serde_json::from_str(body).map_err(|e| RpcError::JsonError(e.to_string()))?;

    let native = resp
        .balances
        .iter()
        .find(|b| b.asset_type == "native")
        .ok_or_else(|| RpcError::JsonError("no native balance entry".to_string()))?;

    // Horizon returns XLM as a decimal string "100.0000000" (7 decimal places).
    let xlm: f64 = native
        .balance
        .parse()
        .map_err(|_| RpcError::JsonError(format!("unparseable balance: {}", native.balance)))?;

    Ok((xlm * 10_000_000.0) as i64)
}

/// Fetch the XLM balance for `account_id` from the Horizon server at
/// `horizon_url`.  Returns the balance in stroops.
pub async fn get_account_balance_stroops(
    horizon_url: &str,
    account_id: &str,
) -> Result<i64, RpcError> {
    let url = format!("{horizon_url}/accounts/{account_id}");

    let response = crate::http::client()
        .get(&url)
        .send()
        .await
        .map_err(|e| RpcError::NetworkError(e.to_string()))?;

    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|e| RpcError::NetworkError(e.to_string()))?;

    if status != 200 {
        return Err(RpcError::HttpError(status));
    }

    parse_account_balance_response(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that a valid `getLatestLedger` RPC response is parsed correctly
    /// and the sequence number is extracted. Closes #409.
    #[test]
    fn parse_valid_latest_ledger_response() {
        let body = r#"{
            "jsonrpc":"2.0","id":1,
            "result":{"id":"abc123","sequence":12345,"protocolVersion":22}
        }"#;
        assert_eq!(parse_latest_ledger_response(body).unwrap(), 12345u32);
    }

    /// Verifies that an RPC fault in the `getLatestLedger` response is
    /// propagated as `RpcError::RpcFault` with the correct code and message.
    /// Closes #410.
    #[test]
    fn parse_rpc_fault_response() {
        let body = r#"{
            "jsonrpc":"2.0","id":1,
            "error":{"code":-32000,"message":"start height out of range"}
        }"#;
        let err = parse_latest_ledger_response(body).unwrap_err();
        assert_eq!(
            err,
            RpcError::RpcFault {
                code: -32000,
                message: "start height out of range".to_string(),
            }
        );
    }

    #[test]
    fn parse_malformed_json_returns_error() {
        let err = parse_latest_ledger_response("not json").unwrap_err();
        assert!(matches!(err, RpcError::JsonError(_)));
    }

    #[test]
    fn parse_missing_result_field() {
        let body = r#"{"jsonrpc":"2.0","id":1}"#;
        let err = parse_latest_ledger_response(body).unwrap_err();
        assert!(matches!(err, RpcError::JsonError(_)));
    }

    // ── account balance ──────────────────────────────────────────────────────

    #[test]
    fn parse_account_balance_native_xlm() {
        let body = r#"{
            "id": "GABC",
            "balances": [
                {"asset_type":"credit_alphanum4","asset_code":"USDC","balance":"50.0000000"},
                {"asset_type":"native","balance":"100.5000000"}
            ]
        }"#;
        let stroops = parse_account_balance_response(body).unwrap();
        assert_eq!(stroops, 1_005_000_000);
    }

    #[test]
    fn parse_account_balance_low_balance() {
        let body = r#"{
            "id": "GABC",
            "balances": [{"asset_type":"native","balance":"0.5000000"}]
        }"#;
        let stroops = parse_account_balance_response(body).unwrap();
        assert_eq!(stroops, 5_000_000);
    }

    #[test]
    fn parse_account_balance_no_native_entry() {
        let body = r#"{
            "id": "GABC",
            "balances": [{"asset_type":"credit_alphanum4","asset_code":"USDC","balance":"10.0"}]
        }"#;
        let err = parse_account_balance_response(body).unwrap_err();
        assert!(matches!(err, RpcError::JsonError(_)));
    }

    #[test]
    fn parse_account_balance_malformed_json() {
        let err = parse_account_balance_response("not json").unwrap_err();
        assert!(matches!(err, RpcError::JsonError(_)));
    }

    // ── get_account_balance_stroops — HTTP-level tests (#404) ────────────────

    #[tokio::test]
    async fn get_account_balance_stroops_200_returns_stroops() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = r#"{
            "id": "GABC",
            "balances": [
                {"asset_type":"native","balance":"50.0000000"}
            ]
        }"#;

        Mock::given(method("GET"))
            .and(path("/accounts/GABC"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let result = get_account_balance_stroops(&server.uri(), "GABC").await;
        assert_eq!(result.unwrap(), 500_000_000); // 50 XLM in stroops
    }

    #[tokio::test]
    async fn get_account_balance_stroops_404_returns_http_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/accounts/GNOT_FOUND"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let err = get_account_balance_stroops(&server.uri(), "GNOT_FOUND")
            .await
            .unwrap_err();
        assert_eq!(err, RpcError::HttpError(404));
    }

    #[tokio::test]
    async fn get_account_balance_stroops_500_returns_http_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/accounts/GABC"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = get_account_balance_stroops(&server.uri(), "GABC")
            .await
            .unwrap_err();
        assert_eq!(err, RpcError::HttpError(500));
    }
}
