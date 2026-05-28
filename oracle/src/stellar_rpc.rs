use serde::{Deserialize, Serialize};
use worker::{Fetch, Headers, Method, Request, RequestInit};

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum RpcError {
    NetworkError(String),
    HttpError(u16),
    JsonError(String),
    RpcFault { code: i64, message: String },
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
    id: String,
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
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
        params: serde_json::Value::Array(vec![]),
    })
    .map_err(|e| RpcError::JsonError(e.to_string()))?;

    let body = rpc_post(rpc_url, payload).await?;
    parse_latest_ledger_response(&body)
}

/// Low-level helper: POST a JSON string to the RPC URL, return the response body.
pub(crate) async fn rpc_post(rpc_url: &str, payload: String) -> Result<String, RpcError> {
    let headers = Headers::new();
    headers
        .set("Content-Type", "application/json")
        .map_err(|e| RpcError::NetworkError(e.to_string()))?;

    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(payload.into()));

    let request = Request::new_with_init(rpc_url, &init)
        .map_err(|e| RpcError::NetworkError(e.to_string()))?;

    let mut response = Fetch::Request(request)
        .send()
        .await
        .map_err(|e| RpcError::NetworkError(e.to_string()))?;

    let status = response.status_code();
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

    let request = worker::Request::new(&url, worker::Method::Get)
        .map_err(|e| RpcError::NetworkError(e.to_string()))?;

    let mut response = worker::Fetch::Request(request)
        .send()
        .await
        .map_err(|e| RpcError::NetworkError(e.to_string()))?;

    let status = response.status_code();
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

    #[test]
    fn parse_valid_latest_ledger_response() {
        let body = r#"{
            "jsonrpc":"2.0","id":1,
            "result":{"id":"abc123","sequence":12345,"protocolVersion":"22"}
        }"#;
        assert_eq!(parse_latest_ledger_response(body).unwrap(), 12345u32);
    }

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
}
