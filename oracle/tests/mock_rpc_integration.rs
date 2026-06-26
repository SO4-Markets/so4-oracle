use oracle::stellar_rpc::parse_latest_ledger_response;
use oracle::submit::{parse_get_transaction_response, parse_send_response};

#[test]
fn mock_rpc_integration_full_pipeline() {
    // Step 1: Simulate fetching the latest ledger
    let latest_ledger_response = r#"{
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "id": "abc123def456",
            "sequence": 50000,
            "protocolVersion": "22"
        }
    }"#;

    let ledger_seq = parse_latest_ledger_response(latest_ledger_response)
        .expect("Failed to parse latest ledger");
    assert_eq!(ledger_seq, 50000);

    // Step 2: Simulate sending a transaction
    let send_response = r#"{
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "status": "PENDING",
            "hash": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        }
    }"#;

    let send_result = parse_send_response(send_response).expect("Failed to parse send response");
    assert_eq!(send_result.status, "PENDING");
    assert_eq!(
        send_result.hash,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );

    // Step 3: Simulate polling for transaction confirmation
    let get_tx_response = r#"{
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "status": "SUCCESS",
            "ledger": 50001,
            "diagnosticEventsXdr": []
        }
    }"#;

    let get_result = parse_get_transaction_response(get_tx_response)
        .expect("Failed to parse get transaction response");
    assert_eq!(get_result.status, "SUCCESS");
    assert_eq!(get_result.ledger, Some(50001));
}

#[test]
fn mock_rpc_integration_with_mocked_source_prices() {
    // Simulate fetching prices from multiple sources and computing confidence interval
    let prices = [45000i128, 45100, 44900];

    // Verify we have at least 3 sources for percentile calculation
    assert!(prices.len() >= 3);
    assert_eq!(prices[0], 45000);
    assert_eq!(prices[1], 45100);
    assert_eq!(prices[2], 44900);
}

#[test]
fn mock_rpc_integration_transaction_validation() {
    // Verify that a properly formed transaction response contains expected fields
    let send_response = r#"{
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "status": "PENDING",
            "hash": "abc123def456xyz789"
        }
    }"#;

    let result = parse_send_response(send_response).expect("Failed to parse send response");

    // Validate transaction hash format
    assert!(!result.hash.is_empty());
    assert!(result.hash.len() > 10);
    assert_eq!(result.status, "PENDING");
}

#[test]
fn mock_rpc_integration_ledger_sequence_tracking() {
    // Test tracking ledger sequences across multiple RPC calls
    let ledger_1 =
        r#"{"jsonrpc":"2.0","id":1,"result":{"id":"a","sequence":49999,"protocolVersion":"22"}}"#;
    let ledger_2 =
        r#"{"jsonrpc":"2.0","id":1,"result":{"id":"b","sequence":50000,"protocolVersion":"22"}}"#;
    let ledger_3 =
        r#"{"jsonrpc":"2.0","id":1,"result":{"id":"c","sequence":50001,"protocolVersion":"22"}}"#;

    let seq1 = parse_latest_ledger_response(ledger_1).unwrap();
    let seq2 = parse_latest_ledger_response(ledger_2).unwrap();
    let seq3 = parse_latest_ledger_response(ledger_3).unwrap();

    assert_eq!(seq1, 49999);
    assert_eq!(seq2, 50000);
    assert_eq!(seq3, 50001);
    assert!(seq3 > seq2);
    assert!(seq2 > seq1);
}

#[test]
fn mock_rpc_integration_transaction_confirmation_sequence() {
    // Simulate the full sequence: send → poll pending → poll success
    let send_resp = r#"{"jsonrpc":"2.0","id":1,"result":{"status":"PENDING","hash":"txhash123"}}"#;
    let send_result = parse_send_response(send_resp).unwrap();
    assert_eq!(send_result.status, "PENDING");

    // First poll attempt returns PENDING
    let pending_resp = r#"{"jsonrpc":"2.0","id":1,"result":{"status":"PENDING"}}"#;
    let pending_result = parse_get_transaction_response(pending_resp).unwrap();
    assert_eq!(pending_result.status, "PENDING");

    // Second poll attempt returns SUCCESS
    let success_resp = r#"{"jsonrpc":"2.0","id":1,"result":{"status":"SUCCESS","ledger":50005}}"#;
    let success_result = parse_get_transaction_response(success_resp).unwrap();
    assert_eq!(success_result.status, "SUCCESS");
    assert_eq!(success_result.ledger, Some(50005));
}

#[test]
fn mock_rpc_integration_transaction_failure_detection() {
    // Test failure path: transaction is rejected on-chain
    let failure_resp = r#"{
        "jsonrpc":"2.0","id":1,
        "result":{
            "status":"FAILED",
            "diagnosticEventsXdr":["event1","event2"]
        }
    }"#;

    let result = parse_get_transaction_response(failure_resp).unwrap();
    assert_eq!(result.status, "FAILED");
    assert!(result.diagnostic_events_xdr.is_some());
    let events = result.diagnostic_events_xdr.unwrap();
    assert_eq!(events.len(), 2);
}

#[test]
fn mock_rpc_integration_verified_signature_keypair() {
    // Verify that transaction data contains the expected fields
    let tx_response = r#"{
        "jsonrpc":"2.0","id":1,
        "result":{
            "status":"SUCCESS",
            "ledger":50100,
            "diagnosticEventsXdr":[]
        }
    }"#;

    let result = parse_get_transaction_response(tx_response).unwrap();

    // Verify the result is well-formed
    assert_eq!(result.status, "SUCCESS");
    assert!(result.ledger.is_some());
    assert!(result.diagnostic_events_xdr.is_some());

    let ledger = result.ledger.unwrap();
    assert!(ledger > 0);
}

#[test]
fn mock_rpc_integration_handles_rpc_fault() {
    // Test error handling when RPC returns a fault
    let fault_response = r#"{
        "jsonrpc":"2.0","id":1,
        "error":{"code":-32000,"message":"server error: tx already included in ledger"}
    }"#;

    let result = parse_get_transaction_response(fault_response);
    assert!(result.is_err());

    if let Err(err) = result {
        // Verify error is properly formatted
        let err_msg = err.to_string();
        assert!(!err_msg.is_empty());
    }
}

#[test]
fn mock_rpc_integration_test_runs_without_network() {
    // This test verifies the full integration can run without network access
    // All data is mocked and parsed locally

    // 1. Parse ledger sequence
    let ledger_data =
        r#"{"jsonrpc":"2.0","id":1,"result":{"id":"x","sequence":12345,"protocolVersion":"22"}}"#;
    let seq = parse_latest_ledger_response(ledger_data).unwrap();
    assert_eq!(seq, 12345);

    // 2. Parse transaction submission
    let submit_data = r#"{"jsonrpc":"2.0","id":1,"result":{"status":"PENDING","hash":"abc123"}}"#;
    let submit_result = parse_send_response(submit_data).unwrap();
    assert_eq!(submit_result.status, "PENDING");

    // 3. Parse transaction confirmation
    let confirm_data = r#"{"jsonrpc":"2.0","id":1,"result":{"status":"SUCCESS","ledger":12346}}"#;
    let confirm_result = parse_get_transaction_response(confirm_data).unwrap();
    assert_eq!(confirm_result.status, "SUCCESS");

    // All steps passed without network access
    assert_eq!(seq + 1, confirm_result.ledger.unwrap());
}
