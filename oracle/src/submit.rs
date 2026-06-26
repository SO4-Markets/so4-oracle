use serde::{Deserialize, Serialize};

use crate::stellar_rpc::{rpc_post, RpcError};

const MAX_POLL_ATTEMPTS: u32 = 10;
const INITIAL_BACKOFF_MS: u64 = 1_000;

#[derive(Debug, PartialEq, Clone)]
pub enum SubmitError {
    Rpc(RpcError),
    JsonError(String),
    Rejected { status: String },
    TransactionFailed { events: Vec<String> },
    PollTimeout,
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubmitError::Rpc(e) => write!(f, "RPC error: {e}"),
            SubmitError::JsonError(msg) => write!(f, "JSON parse error: {msg}"),
            SubmitError::Rejected { status } => write!(f, "transaction rejected: {status}"),
            SubmitError::TransactionFailed { events } => {
                write!(
                    f,
                    "transaction failed on-chain; diagnostic events: {events:?}"
                )
            }
            SubmitError::PollTimeout => write!(
                f,
                "transaction not confirmed after {MAX_POLL_ATTEMPTS} attempts"
            ),
        }
    }
}

// ── JSON-RPC wire types ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct JsonRpcRequest<'a, P: Serialize> {
    jsonrpc: &'a str,
    id: u32,
    method: &'a str,
    params: P,
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

// ── sendTransaction response ─────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct SendTransactionResult {
    pub status: String,
    pub hash: String,
    #[serde(rename = "errorResultXdr", default)]
    pub error_result_xdr: Option<String>,
}

/// Parse the raw body of a `sendTransaction` RPC response.
pub fn parse_send_response(body: &str) -> Result<SendTransactionResult, SubmitError> {
    let resp: JsonRpcResponse<SendTransactionResult> =
        serde_json::from_str(body).map_err(|e| SubmitError::JsonError(e.to_string()))?;

    if let Some(fault) = resp.error {
        return Err(SubmitError::Rpc(RpcError::RpcFault {
            code: fault.code,
            message: fault.message,
        }));
    }

    resp.result
        .ok_or_else(|| SubmitError::JsonError("missing 'result' field".to_string()))
}

// ── getTransaction response ──────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct GetTransactionResult {
    pub status: String,
    #[serde(default)]
    pub ledger: Option<u32>,
    #[serde(rename = "diagnosticEventsXdr", default)]
    pub diagnostic_events_xdr: Option<Vec<String>>,
}

/// Parse the raw body of a `getTransaction` RPC response.
pub fn parse_get_transaction_response(body: &str) -> Result<GetTransactionResult, SubmitError> {
    let resp: JsonRpcResponse<GetTransactionResult> =
        serde_json::from_str(body).map_err(|e| SubmitError::JsonError(e.to_string()))?;

    if let Some(fault) = resp.error {
        return Err(SubmitError::Rpc(RpcError::RpcFault {
            code: fault.code,
            message: fault.message,
        }));
    }

    resp.result
        .ok_or_else(|| SubmitError::JsonError("missing 'result' field".to_string()))
}

// ── Async submission + polling ───────────────────────────────────────────────

/// Submit a base64-encoded signed transaction XDR and return the transaction hash.
async fn send_transaction_xdr(rpc_url: &str, signed_xdr: &str) -> Result<String, SubmitError> {
    let payload = serde_json::to_string(&JsonRpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "sendTransaction",
        params: serde_json::json!({ "transaction": signed_xdr }),
    })
    .map_err(|e| SubmitError::JsonError(e.to_string()))?;

    let body = rpc_post(rpc_url, payload).await.map_err(SubmitError::Rpc)?;

    let result = parse_send_response(&body)?;

    if result.status != "PENDING" {
        return Err(SubmitError::Rejected {
            status: result.status,
        });
    }

    Ok(result.hash)
}

/// Poll `getTransaction` until confirmed or until `MAX_POLL_ATTEMPTS` are exhausted.
///
/// Delay strategy: exponential backoff starting at `INITIAL_BACKOFF_MS`.
async fn poll_until_confirmed(rpc_url: &str, hash: &str) -> Result<u32, SubmitError> {
    let mut backoff_ms = INITIAL_BACKOFF_MS;

    for attempt in 0..MAX_POLL_ATTEMPTS {
        let payload = serde_json::to_string(&JsonRpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "getTransaction",
            params: serde_json::json!({ "hash": hash }),
        })
        .map_err(|e| SubmitError::JsonError(e.to_string()))?;

        let body = rpc_post(rpc_url, payload).await.map_err(SubmitError::Rpc)?;

        let result = parse_get_transaction_response(&body)?;

        match result.status.as_str() {
            "SUCCESS" => {
                let ledger = result.ledger.unwrap_or(0);
                tracing::info!(hash, ledger, "transaction confirmed");
                return Ok(ledger);
            }
            "FAILED" => {
                let events = result.diagnostic_events_xdr.unwrap_or_default();
                tracing::error!(hash, ?events, "transaction failed");
                return Err(SubmitError::TransactionFailed { events });
            }
            // "NOT_FOUND" or "PENDING" — keep waiting
            _ => {
                tracing::debug!(
                    hash,
                    status = result.status,
                    attempt,
                    max_attempts = MAX_POLL_ATTEMPTS,
                    next_backoff_ms = backoff_ms,
                    "transaction still pending"
                );
                sleep_ms(backoff_ms).await;
                // Double the backoff for the next iteration (capped at 30 s).
                backoff_ms = (backoff_ms * 2).min(30_000);
            }
        }
    }

    Err(SubmitError::PollTimeout)
}

async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

/// Submit a signed transaction XDR and poll for the result with exponential backoff.
///
/// Returns the ledger sequence at which the transaction was confirmed.
pub async fn submit_and_poll(rpc_url: &str, signed_xdr: &str) -> Result<u32, SubmitError> {
    let hash = send_transaction_xdr(rpc_url, signed_xdr).await?;
    tracing::info!(hash, "transaction submitted");
    poll_until_confirmed(rpc_url, &hash).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── sendTransaction parsing ──────────────────────────────────────────────

    /// Verifies that a PENDING sendTransaction response is parsed correctly.
    /// Closes #423.
    #[test]
    fn parse_send_response_pending() {
        let body = r#"{
            "jsonrpc":"2.0","id":1,
            "result":{"status":"PENDING","hash":"abc123def456"}
        }"#;
        let r = parse_send_response(body).unwrap();
        assert_eq!(r.status, "PENDING");
        assert_eq!(r.hash, "abc123def456");
    }

    /// Verifies that an ERROR status with errorResultXdr in a sendTransaction
    /// response is parsed correctly. Closes #424.
    #[test]
    fn parse_send_response_error_status() {
        let body = r#"{
            "jsonrpc":"2.0","id":1,
            "result":{"status":"ERROR","hash":"abc123","errorResultXdr":"AAAA"}
        }"#;
        let r = parse_send_response(body).unwrap();
        assert_eq!(r.status, "ERROR");
        assert_eq!(r.error_result_xdr.as_deref(), Some("AAAA"));
    }

    /// Verifies that an RPC fault in a sendTransaction response is propagated
    /// as SubmitError::Rpc(RpcError::RpcFault). Closes #425.
    #[test]
    fn parse_send_response_rpc_fault() {
        let body = r#"{
            "jsonrpc":"2.0","id":1,
            "error":{"code":-32600,"message":"invalid request"}
        }"#;
        let err = parse_send_response(body).unwrap_err();
        assert!(matches!(err, SubmitError::Rpc(RpcError::RpcFault { .. })));
    }

    // ── getTransaction parsing ───────────────────────────────────────────────

    /// Verifies that a SUCCESS getTransaction response with ledger sequence is
    /// parsed correctly. Closes #426.
    #[test]
    fn parse_get_transaction_success() {
        let body = r#"{
            "jsonrpc":"2.0","id":1,
            "result":{"status":"SUCCESS","ledger":99,"diagnosticEventsXdr":[]}
        }"#;
        let r = parse_get_transaction_response(body).unwrap();
        assert_eq!(r.status, "SUCCESS");
        assert_eq!(r.ledger, Some(99));
    }

    #[test]
    fn parse_get_transaction_failed_with_events() {
        let body = r#"{
            "jsonrpc":"2.0","id":1,
            "result":{
                "status":"FAILED",
                "diagnosticEventsXdr":["event_xdr_1","event_xdr_2"]
            }
        }"#;
        let r = parse_get_transaction_response(body).unwrap();
        assert_eq!(r.status, "FAILED");
        let events = r.diagnostic_events_xdr.unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn parse_get_transaction_not_found() {
        let body = r#"{
            "jsonrpc":"2.0","id":1,
            "result":{"status":"NOT_FOUND"}
        }"#;
        let r = parse_get_transaction_response(body).unwrap();
        assert_eq!(r.status, "NOT_FOUND");
    }

    #[test]
    fn parse_get_transaction_malformed_json() {
        let err = parse_get_transaction_response("garbage").unwrap_err();
        assert!(matches!(err, SubmitError::JsonError(_)));
    }
}
