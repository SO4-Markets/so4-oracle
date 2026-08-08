use crate::retry::Retryable;
use serde::{Deserialize, Serialize};

use crate::stellar_rpc::{rpc_post, RpcError};

const MAX_POLL_ATTEMPTS: u32 = 10;
#[cfg(not(test))]
const INITIAL_BACKOFF_MS: u64 = 1_000;
#[cfg(test)]
const INITIAL_BACKOFF_MS: u64 = 1;

/// Maximum number of diagnostic-event XDR entries logged at warn/error level.
/// Full payload capture is already available in the admin-gated failure ring
/// buffer; the structured log only needs enough context for triage.
const MAX_LOGGED_EVENTS: usize = 5;
/// Maximum character length of a single diagnostic-event XDR entry in logs.
const MAX_EVENT_PREVIEW_LEN: usize = 128;

/// Truncate a diagnostic-events vector for structured logging.
///
/// Returns at most `MAX_LOGGED_EVENTS` entries, each capped at
/// `MAX_EVENT_PREVIEW_LEN` characters with a `…` suffix when truncated.
/// Logs a count of the total events so operators know how many were dropped.
fn truncate_events_for_log(events: &[String]) -> Vec<String> {
    let total = events.len();
    let truncated: Vec<String> = events
        .iter()
        .take(MAX_LOGGED_EVENTS)
        .map(|e| {
            if e.len() > MAX_EVENT_PREVIEW_LEN {
                format!("{}…", &e[..MAX_EVENT_PREVIEW_LEN])
            } else {
                e.clone()
            }
        })
        .collect();

    if total > MAX_LOGGED_EVENTS {
        tracing::debug!(
            total_events = total,
            logged_events = truncated.len(),
            "diagnostic events truncated for log; full payload in failure ring buffer"
        );
    }

    truncated
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum SubmitError {
    Rpc(RpcError),
    JsonError(String),
    Rejected { status: String },
    TransactionFailed { events: Vec<String> },
    PollTimeout { hash: String },
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
            SubmitError::PollTimeout { hash } => write!(
                f,
                "transaction not confirmed after {MAX_POLL_ATTEMPTS} attempts (hash: {hash}); check status on next cycle"
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

        // Call RPC and parse response, but route transient RPC/network errors
        // through the same backoff/retry path used for PENDING/NOT_FOUND so
        // a single transient blip doesn't abort the whole poll.
        let body = match rpc_post(rpc_url, payload).await {
            Ok(b) => b,
            Err(rpc_err) => {
                // Map to SubmitError for logging/returning when non-retryable.
                if rpc_err.is_retryable() {
                    tracing::warn!(
                        hash,
                        ?rpc_err,
                        attempt,
                        "transient RPC/network error; will retry"
                    );
                    sleep_ms(backoff_ms).await;
                    backoff_ms = (backoff_ms * 2).min(30_000);
                    continue;
                } else {
                    return Err(SubmitError::Rpc(rpc_err));
                }
            }
        };

        let result = match parse_get_transaction_response(&body) {
            Ok(r) => r,
            Err(e) => match e {
                // Propagate non-retryable parse/RPC faults immediately.
                SubmitError::Rpc(rpc_err) => return Err(SubmitError::Rpc(rpc_err)),
                SubmitError::JsonError(_) => return Err(e),
                // Any other SubmitError variants shouldn't occur here, but
                // treat them as non-retryable and return.
                _ => return Err(e),
            },
        };

        match result.status.as_str() {
            "SUCCESS" => {
                let ledger = result.ledger.unwrap_or(0);
                tracing::info!(hash, ledger, "transaction confirmed");
                return Ok(ledger);
            }
            "FAILED" => {
                let events = result.diagnostic_events_xdr.unwrap_or_default();
                let preview = truncate_events_for_log(&events);
                tracing::warn!(
                    hash,
                    event_count = events.len(),
                    ?preview,
                    "transaction failed"
                );
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

    tracing::warn!(
        hash,
        "transaction not confirmed after {MAX_POLL_ATTEMPTS} attempts; will check status on next cycle"
    );
    Err(SubmitError::PollTimeout {
        hash: hash.to_string(),
    })
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
    use serde_json::json;
    use wiremock::ResponseTemplate;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer,
    };

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
        let err = SubmitError::PollTimeout {
            hash: "abc123def456".to_string(),
        };
        assert!(err.to_string().contains("not confirmed after 10 attempts"));
        assert!(err.to_string().contains("abc123def456"));
    }

    #[test]
    fn submit_error_from_rpc_error() {
        let rpc_err = RpcError::HttpError {
            status: 503,
            body: String::new(),
        };
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

        assert!(matches!(err, SubmitError::PollTimeout { .. }));
        if let SubmitError::PollTimeout { hash } = err {
            assert_eq!(hash, "abc123def456");
        }
    }

    #[tokio::test]
    async fn poll_unrecognized_status_continues_until_success() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(rpc_responder(vec![
                serde_json::json!({ "status": "UNKNOWN_STATUS" }),
                serde_json::json!({ "status": "SUCCESS", "ledger": 77 }),
            ]))
            .mount(&mock)
            .await;

        let ledger = submit_and_poll(&mock.uri(), "signed_xdr_base64")
            .await
            .unwrap();

        assert_eq!(ledger, 77);
        assert_eq!(mock.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn poll_timeout_includes_hash_for_later_reconciliation() {
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

        if let SubmitError::PollTimeout { hash } = err {
            assert_eq!(
                hash, "abc123def456",
                "hash should be available for reconciliation"
            );
        } else {
            panic!("expected PollTimeout error with hash");
        }
    }

    #[tokio::test]
    async fn test_submit_and_poll_rejected_on_non_pending_send() {
        let mock_server = MockServer::start().await;

        let send_response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "status": "REJECTED",
                "hash": "abc123def456",
                "errorResultXdr": "AAAAAA=="
            }
        });

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(send_response_body))
            .expect(1)
            .mount(&mock_server)
            .await;

        let signed_xdr = "AAAAAA==";
        let rpc_url = mock_server.uri();

        let result = submit_and_poll(&rpc_url, signed_xdr).await;

        match result {
            Err(SubmitError::Rejected { status }) => {
                assert_eq!(status, "REJECTED");
            }
            other => panic!("Expected SubmitError::Rejected, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_submit_and_poll_rejected_with_various_status_codes() {
        let test_cases = vec![
            "REJECTED",
            "FAILED",
            "TIMEOUT",
            "INSUFFICIENT_FEE",
            "BAD_SEQUENCE",
        ];

        for status in test_cases {
            let mock_server = MockServer::start().await;

            let send_response_body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "status": status,
                    "hash": "abc123def456",
                }
            });

            Mock::given(method("POST"))
                .and(path("/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(send_response_body))
                .expect(1)
                .mount(&mock_server)
                .await;

            let signed_xdr = "AAAAAA==";
            let rpc_url = mock_server.uri();

            let result = submit_and_poll(&rpc_url, signed_xdr).await;

            match result {
                Err(SubmitError::Rejected {
                    status: returned_status,
                }) => {
                    assert_eq!(returned_status, status);
                }
                other => {
                    panic!("Expected SubmitError::Rejected with status '{status}', got: {other:?}")
                }
            }
        }
    }

    #[tokio::test]
    async fn test_submit_and_poll_success_path_with_pending_send() {
        let mock_server = MockServer::start().await;

        use std::sync::atomic::{AtomicUsize, Ordering};
        let request_count = AtomicUsize::new(0);

        let send_response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "status": "PENDING",
                "hash": "abc123def456"
            }
        });

        let get_response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "status": "SUCCESS",
                "ledger": 12345
            }
        });

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(move |_: &wiremock::Request| {
                let count = request_count.fetch_add(1, Ordering::SeqCst);
                if count == 0 {
                    ResponseTemplate::new(200).set_body_json(send_response.clone())
                } else {
                    ResponseTemplate::new(200).set_body_json(get_response.clone())
                }
            })
            .expect(2)
            .mount(&mock_server)
            .await;

        let signed_xdr = "AAAAAA==";
        let rpc_url = mock_server.uri();

        let result = submit_and_poll(&rpc_url, signed_xdr).await;

        assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
        assert_eq!(result.unwrap(), 12345);
    }

    #[tokio::test]
    async fn test_submit_and_poll_handles_send_transaction_rpc_error() {
        let mock_server = MockServer::start().await;

        let error_response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32000,
                "message": "Internal error"
            }
        });

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(error_response_body))
            .expect(1)
            .mount(&mock_server)
            .await;

        let signed_xdr = "AAAAAA==";
        let rpc_url = mock_server.uri();

        let result = submit_and_poll(&rpc_url, signed_xdr).await;

        match result {
            Err(SubmitError::Rpc(RpcError::RpcFault { code, message })) => {
                assert_eq!(code, -32000);
                assert_eq!(message, "Internal error");
            }
            other => panic!("Expected SubmitError::Rpc, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_submit_and_poll_handles_malformed_send_response() {
        let mock_server = MockServer::start().await;

        let malformed_response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1
        });

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(malformed_response))
            .expect(1)
            .mount(&mock_server)
            .await;

        let signed_xdr = "AAAAAA==";
        let rpc_url = mock_server.uri();

        let result = submit_and_poll(&rpc_url, signed_xdr).await;

        match result {
            Err(SubmitError::JsonError(msg)) => {
                assert!(msg.contains("missing 'result' field"));
            }
            other => panic!("Expected SubmitError::JsonError, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_submit_and_poll_does_not_poll_on_rejection() {
        let mock_server = MockServer::start().await;

        let send_response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "status": "REJECTED",
                "hash": "abc123def456"
            }
        });

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(send_response_body))
            .expect(1)
            .mount(&mock_server)
            .await;

        let signed_xdr = "AAAAAA==";
        let rpc_url = mock_server.uri();

        let result = submit_and_poll(&rpc_url, signed_xdr).await;

        assert!(matches!(result, Err(SubmitError::Rejected { .. })));
    }

    #[tokio::test]
    async fn test_submit_and_poll_rejected_with_error_result_xdr() {
        let mock_server = MockServer::start().await;

        let send_response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "status": "REJECTED",
                "hash": "abc123def456",
                "errorResultXdr": "AAAAAA=="
            }
        });

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(send_response_body))
            .expect(1)
            .mount(&mock_server)
            .await;

        let signed_xdr = "AAAAAA==";
        let rpc_url = mock_server.uri();

        let result = submit_and_poll(&rpc_url, signed_xdr).await;

        match result {
            Err(SubmitError::Rejected { status }) => {
                assert_eq!(status, "REJECTED");
            }
            other => panic!("Expected SubmitError::Rejected, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_submit_and_poll_handles_http_error_on_send() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let signed_xdr = "AAAAAA==";
        let rpc_url = mock_server.uri();

        let result = submit_and_poll(&rpc_url, signed_xdr).await;

        assert!(matches!(result, Err(SubmitError::Rpc(_))));
    }

    #[tokio::test]
    async fn test_parse_send_response_handles_non_pending_status() {
        let test_cases = vec![
            ("REJECTED", "REJECTED"),
            ("FAILED", "FAILED"),
            ("TIMEOUT", "TIMEOUT"),
            ("INSUFFICIENT_FEE", "INSUFFICIENT_FEE"),
        ];

        for (status, expected_status) in test_cases {
            let response_body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "status": status,
                    "hash": "test_hash"
                }
            });

            let result = parse_send_response(&response_body.to_string()).unwrap();
            assert_eq!(result.status, expected_status);
            assert_eq!(result.hash, "test_hash");
        }
    }
}
