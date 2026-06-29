use serde::{Deserialize, Serialize};

use crate::stellar_rpc::{rpc_post, RpcError};

const MAX_POLL_ATTEMPTS: u32 = 10;
#[cfg(not(test))]
const INITIAL_BACKOFF_MS: u64 = 1_000;
#[cfg(test)]
const INITIAL_BACKOFF_MS: u64 = 1;

#[derive(Debug, PartialEq, Eq, Clone)]
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

impl From<RpcError> for SubmitError {
    fn from(err: RpcError) -> Self {
        SubmitError::Rpc(err)
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
            "PENDING" | "NOT_FOUND" => {
                tracing::debug!(
                    hash,
                    status = result.status,
                    attempt,
                    max_attempts = MAX_POLL_ATTEMPTS,
                    next_backoff_ms = backoff_ms,
                    "transaction still pending"
                );
                sleep_ms(backoff_ms).await;
                backoff_ms = (backoff_ms * 2).min(30_000);
            }
            _ => {
                tracing::warn!(
                    hash,
                    status = result.status,
                    attempt,
                    "unexpected transaction status; continuing poll"
                );
                sleep_ms(backoff_ms).await;
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
#[tracing::instrument(skip_all, fields(hash = tracing::field::Empty))]
pub async fn submit_and_poll(rpc_url: &str, signed_xdr: &str) -> Result<u32, SubmitError> {
    let hash = send_transaction_xdr(rpc_url, signed_xdr).await?;
    tracing::Span::current().record("hash", tracing::field::display(&hash));
    tracing::info!(hash, "transaction submitted");
    poll_until_confirmed(rpc_url, &hash).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::ResponseTemplate;

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

    /// Verifies that an RPC fault in a getTransaction response is propagated
    /// as SubmitError::Rpc(RpcError::RpcFault). Closes #427.
    #[test]
    fn parse_get_transaction_rpc_fault() {
        let body = r#"{
            "jsonrpc":"2.0","id":1,
            "error":{"code":-32600,"message":"invalid request"}
        }"#;
        let err = parse_get_transaction_response(body).unwrap_err();
        assert!(matches!(err, SubmitError::Rpc(RpcError::RpcFault { .. })));
    }

    /// Verifies that a NOT_FOUND getTransaction response is parsed without error.
    /// Closes #428.
    #[test]
    fn parse_get_transaction_not_found() {
        let body = r#"{
            "jsonrpc":"2.0","id":1,
            "result":{"status":"NOT_FOUND"}
        }"#;
        let r = parse_get_transaction_response(body).unwrap();
        assert_eq!(r.status, "NOT_FOUND");
    }

    /// Verifies that malformed JSON input returns a JsonError without panicking.
    /// Closes #429.
    #[test]
    fn parse_get_transaction_malformed_json() {
        let err = parse_get_transaction_response("garbage").unwrap_err();
        assert!(matches!(err, SubmitError::JsonError(_)));
    }

    // ── SubmitError Display ──────────────────────────────────────────────────

    #[test]
    fn submit_error_display_rpc() {
        let err = SubmitError::Rpc(RpcError::NetworkError("connection reset".to_string()));
        assert_eq!(
            err.to_string(),
            "RPC error: network error: connection reset"
        );
    }

    #[test]
    fn submit_error_display_json_error() {
        let err = SubmitError::JsonError("unexpected token".to_string());
        assert_eq!(err.to_string(), "JSON parse error: unexpected token");
    }

    #[test]
    fn submit_error_display_rejected() {
        let err = SubmitError::Rejected {
            status: "ERROR".to_string(),
        };
        assert_eq!(err.to_string(), "transaction rejected: ERROR");
    }

    #[test]
    fn submit_error_display_transaction_failed() {
        let events = vec!["event_xdr_1".to_string(), "event_xdr_2".to_string()];
        let err = SubmitError::TransactionFailed {
            events: events.clone(),
        };
        assert_eq!(
            err.to_string(),
            format!("transaction failed on-chain; diagnostic events: {events:?}")
        );
    }

    #[test]
    fn submit_error_display_poll_timeout() {
        let err = SubmitError::PollTimeout;
        assert_eq!(
            err.to_string(),
            "transaction not confirmed after 10 attempts"
        );
    }

    #[test]
    fn submit_error_from_rpc_error() {
        let rpc_err = RpcError::HttpError(503);
        let submit_err: SubmitError = rpc_err.clone().into();
        assert_eq!(submit_err, SubmitError::Rpc(rpc_err));
    }

    // ── poll_until_confirmed integration (wiremock) ──────────────────────────

    fn rpc_responder(
        get_transaction_bodies: Vec<serde_json::Value>,
    ) -> impl Fn(&wiremock::Request) -> ResponseTemplate {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let counter = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(get_transaction_bodies);

        move |req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let method = body["method"].as_str().unwrap_or("");

            match method {
                "sendTransaction" => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "status": "PENDING",
                        "hash": "abc123def456"
                    }
                })),
                "getTransaction" => {
                    let idx = counter.fetch_add(1, Ordering::SeqCst);
                    let result = bodies.get(idx).cloned().unwrap_or_else(
                        || serde_json::json!({ "status": "SUCCESS", "ledger": 42 }),
                    );
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": result
                    }))
                }
                _ => ResponseTemplate::new(400),
            }
        }
    }

    #[tokio::test]
    async fn poll_failed_returns_error_with_events() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(rpc_responder(vec![serde_json::json!({
                "status": "FAILED",
                "diagnosticEventsXdr": ["event_xdr_1", "event_xdr_2"]
            })]))
            .mount(&mock)
            .await;

        let err = submit_and_poll(&mock.uri(), "signed_xdr_base64")
            .await
            .unwrap_err();

        assert_eq!(
            err,
            SubmitError::TransactionFailed {
                events: vec!["event_xdr_1".to_string(), "event_xdr_2".to_string()],
            }
        );
    }

    #[tokio::test]
    async fn poll_pending_continues_until_success() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(rpc_responder(vec![
                serde_json::json!({ "status": "PENDING" }),
                serde_json::json!({ "status": "PENDING" }),
                serde_json::json!({ "status": "SUCCESS", "ledger": 99 }),
            ]))
            .mount(&mock)
            .await;

        let ledger = submit_and_poll(&mock.uri(), "signed_xdr_base64")
            .await
            .unwrap();

        assert_eq!(ledger, 99);
        // sendTransaction + 3 getTransaction polls
        assert_eq!(mock.received_requests().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn poll_not_found_continues_until_success() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(rpc_responder(vec![
                serde_json::json!({ "status": "NOT_FOUND" }),
                serde_json::json!({ "status": "SUCCESS", "ledger": 55 }),
            ]))
            .mount(&mock)
            .await;

        let ledger = submit_and_poll(&mock.uri(), "signed_xdr_base64")
            .await
            .unwrap();

        assert_eq!(ledger, 55);
        assert_eq!(mock.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn poll_exhausted_returns_poll_timeout() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer};

        let mock = MockServer::start().await;
        let pending = serde_json::json!({ "status": "PENDING" });
        Mock::given(method("POST"))
            .respond_with(rpc_responder(vec![pending; MAX_POLL_ATTEMPTS as usize]))
            .mount(&mock)
            .await;

        let err = submit_and_poll(&mock.uri(), "signed_xdr_base64")
            .await
            .unwrap_err();

        assert_eq!(err, SubmitError::PollTimeout);
    }
}
