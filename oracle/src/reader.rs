use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderCount {
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderKey {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderKeys {
    pub keys: Vec<OrderKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawalCount {
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawalKey {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawalKeys {
    pub keys: Vec<WithdrawalKey>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReaderError {
    SimulationError(String),
    RpcError(String),
    DecodeError(String),
}

impl std::fmt::Display for ReaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReaderError::SimulationError(msg) => write!(f, "simulation error: {msg}"),
            ReaderError::RpcError(msg) => write!(f, "RPC error: {msg}"),
            ReaderError::DecodeError(msg) => write!(f, "decode error: {msg}"),
        }
    }
}

pub async fn get_order_count(
    rpc_url: &str,
    reader_contract_id: &str,
    data_store_id: &str,
) -> Result<u32, ReaderError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "simulateTransaction",
        "params": {
            "transaction": {
                "source_account": data_store_id,
                "fee": "100",
                "operations": [{
                    "type": "invoke",
                    "contract": reader_contract_id,
                    "function": "get_order_count",
                    "args": [data_store_id]
                }]
            }
        }
    });

    let body = crate::stellar_rpc::rpc_post(rpc_url, payload.to_string())
        .await
        .map_err(|e| ReaderError::RpcError(e.to_string()))?;

    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| ReaderError::SimulationError(e.to_string()))?;

    if let Some(error) = resp.get("error") {
        return Err(ReaderError::SimulationError(error.to_string()));
    }

    let result = resp
        .get("result")
        .ok_or_else(|| ReaderError::SimulationError("missing result".to_string()))?;

    let count = result
        .get("transactionResult")
        .and_then(|r| r.as_str())
        .and_then(|r| r.parse::<u32>().ok())
        .unwrap_or(0);

    Ok(count)
}

pub async fn get_order_keys(
    rpc_url: &str,
    reader_contract_id: &str,
    data_store_id: &str,
    start: u32,
    count: u32,
) -> Result<Vec<String>, ReaderError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "simulateTransaction",
        "params": {
            "transaction": {
                "source_account": data_store_id,
                "fee": "100",
                "operations": [{
                    "type": "invoke",
                    "contract": reader_contract_id,
                    "function": "get_order_keys",
                    "args": [data_store_id, start, count]
                }]
            }
        }
    });

    let body = crate::stellar_rpc::rpc_post(rpc_url, payload.to_string())
        .await
        .map_err(|e| ReaderError::RpcError(e.to_string()))?;

    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| ReaderError::SimulationError(e.to_string()))?;

    if let Some(error) = resp.get("error") {
        return Err(ReaderError::SimulationError(error.to_string()));
    }

    let result = resp
        .get("result")
        .ok_or_else(|| ReaderError::SimulationError("missing result".to_string()))?;

    let keys = result
        .get("transactionResult")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok(keys)
}

pub async fn get_withdrawal_count(
    rpc_url: &str,
    reader_contract_id: &str,
    data_store_id: &str,
) -> Result<u32, ReaderError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "simulateTransaction",
        "params": {
            "transaction": {
                "source_account": data_store_id,
                "fee": "100",
                "operations": [{
                    "type": "invoke",
                    "contract": reader_contract_id,
                    "function": "get_withdrawal_count",
                    "args": [data_store_id]
                }]
            }
        }
    });

    let body = crate::stellar_rpc::rpc_post(rpc_url, payload.to_string())
        .await
        .map_err(|e| ReaderError::RpcError(e.to_string()))?;

    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| ReaderError::SimulationError(e.to_string()))?;

    if let Some(error) = resp.get("error") {
        return Err(ReaderError::SimulationError(error.to_string()));
    }

    let result = resp
        .get("result")
        .ok_or_else(|| ReaderError::SimulationError("missing result".to_string()))?;

    let count = result
        .get("transactionResult")
        .and_then(|r| r.as_str())
        .and_then(|r| r.parse::<u32>().ok())
        .unwrap_or(0);

    Ok(count)
}

pub async fn get_withdrawal_keys(
    rpc_url: &str,
    reader_contract_id: &str,
    data_store_id: &str,
    start: u32,
    count: u32,
) -> Result<Vec<String>, ReaderError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "simulateTransaction",
        "params": {
            "transaction": {
                "source_account": data_store_id,
                "fee": "100",
                "operations": [{
                    "type": "invoke",
                    "contract": reader_contract_id,
                    "function": "get_withdrawal_keys",
                    "args": [data_store_id, start, count]
                }]
            }
        }
    });

    let body = crate::stellar_rpc::rpc_post(rpc_url, payload.to_string())
        .await
        .map_err(|e| ReaderError::RpcError(e.to_string()))?;

    let resp: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| ReaderError::SimulationError(e.to_string()))?;

    if let Some(error) = resp.get("error") {
        return Err(ReaderError::SimulationError(error.to_string()));
    }

    let result = resp
        .get("result")
        .ok_or_else(|| ReaderError::SimulationError("missing result".to_string()))?;

    let keys = result
        .get("transactionResult")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn test_order_count_deserialize() {
        let json = r#"{"count": 5}"#;
        let count: OrderCount = serde_json::from_str(json).unwrap();
        assert_eq!(count.count, 5);
    }

    #[test]
    fn test_order_keys_deserialize() {
        let json = r#"{"keys": [{"key": "key1"}, {"key": "key2"}]}"#;
        let keys: OrderKeys = serde_json::from_str(json).unwrap();
        assert_eq!(keys.keys.len(), 2);
    }

    #[test]
    fn test_withdrawal_count_deserialize() {
        let json = r#"{"count": 3}"#;
        let count: WithdrawalCount = serde_json::from_str(json).unwrap();
        assert_eq!(count.count, 3);
    }

    #[tokio::test]
    async fn test_get_order_count_success() {
        let server = MockServer::start().await;
        let response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "transactionResult": "42"
            }
        });

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let count = get_order_count(&server.uri(), "reader_contract", "data_store")
            .await
            .unwrap();
        assert_eq!(count, 42);
    }

    #[tokio::test]
    async fn test_get_order_count_rpc_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = get_order_count(&server.uri(), "reader_contract", "data_store")
            .await
            .unwrap_err();
        assert!(matches!(err, ReaderError::RpcError(_)));
    }

    #[tokio::test]
    async fn test_get_order_count_simulation_error() {
        let server = MockServer::start().await;
        let response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32600,
                "message": "Simulation failed"
            }
        });

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let err = get_order_count(&server.uri(), "reader_contract", "data_store")
            .await
            .unwrap_err();
        assert!(matches!(err, ReaderError::SimulationError(_)));
    }

    #[tokio::test]
    async fn test_get_order_count_missing_result() {
        let server = MockServer::start().await;
        let response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1
        });

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let err = get_order_count(&server.uri(), "reader_contract", "data_store")
            .await
            .unwrap_err();
        assert_eq!(err, ReaderError::SimulationError("missing result".to_string()));
    }

    #[tokio::test]
    async fn test_get_order_keys_success() {
        let server = MockServer::start().await;
        let response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "transactionResult": ["key1", "key2", "key3"]
            }
        });

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let keys = get_order_keys(&server.uri(), "reader_contract", "data_store", 0, 3)
            .await
            .unwrap();
        assert_eq!(keys, vec!["key1".to_string(), "key2".to_string(), "key3".to_string()]);
    }

    #[tokio::test]
    async fn test_get_order_keys_rpc_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = get_order_keys(&server.uri(), "reader_contract", "data_store", 0, 3)
            .await
            .unwrap_err();
        assert!(matches!(err, ReaderError::RpcError(_)));
    }

    #[tokio::test]
    async fn test_get_order_keys_simulation_error() {
        let server = MockServer::start().await;
        let response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": "Failed execution"
        });

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let err = get_order_keys(&server.uri(), "reader_contract", "data_store", 0, 3)
            .await
            .unwrap_err();
        assert!(matches!(err, ReaderError::SimulationError(_)));
    }

    #[tokio::test]
    async fn test_get_order_keys_missing_result() {
        let server = MockServer::start().await;
        let response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1
        });

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let err = get_order_keys(&server.uri(), "reader_contract", "data_store", 0, 3)
            .await
            .unwrap_err();
        assert_eq!(err, ReaderError::SimulationError("missing result".to_string()));
    }

    #[tokio::test]
    async fn test_get_withdrawal_count_success() {
        let server = MockServer::start().await;
        let response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "transactionResult": "100"
            }
        });

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let count = get_withdrawal_count(&server.uri(), "reader_contract", "data_store")
            .await
            .unwrap();
        assert_eq!(count, 100);
    }

    #[tokio::test]
    async fn test_get_withdrawal_count_rpc_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = get_withdrawal_count(&server.uri(), "reader_contract", "data_store")
            .await
            .unwrap_err();
        assert!(matches!(err, ReaderError::RpcError(_)));
    }

    #[tokio::test]
    async fn test_get_withdrawal_count_simulation_error() {
        let server = MockServer::start().await;
        let response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": "Error details"
        });

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let err = get_withdrawal_count(&server.uri(), "reader_contract", "data_store")
            .await
            .unwrap_err();
        assert!(matches!(err, ReaderError::SimulationError(_)));
    }

    #[tokio::test]
    async fn test_get_withdrawal_keys_success() {
        let server = MockServer::start().await;
        let response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "transactionResult": ["wkey1", "wkey2"]
            }
        });

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let keys = get_withdrawal_keys(&server.uri(), "reader_contract", "data_store", 0, 2)
            .await
            .unwrap();
        assert_eq!(keys, vec!["wkey1".to_string(), "wkey2".to_string()]);
    }

    #[tokio::test]
    async fn test_get_withdrawal_keys_rpc_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = get_withdrawal_keys(&server.uri(), "reader_contract", "data_store", 0, 2)
            .await
            .unwrap_err();
        assert!(matches!(err, ReaderError::RpcError(_)));
    }

    #[tokio::test]
    async fn test_get_withdrawal_keys_simulation_error() {
        let server = MockServer::start().await;
        let response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": "Error details"
        });

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let err = get_withdrawal_keys(&server.uri(), "reader_contract", "data_store", 0, 2)
            .await
            .unwrap_err();
        assert!(matches!(err, ReaderError::SimulationError(_)));
    }
}
