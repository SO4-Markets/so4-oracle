//! Parsers and RPC clients run against full Soroban RPC / Horizon response
//! shapes (#1423), not the minimal subset hand-rolled mocks tend to contain.
//! See `tests/fixtures/README.md` for where these files come from and what
//! they do not cover yet.

use stellar_xdr::{Limits, ReadXdr, TransactionResult, TransactionResultResult};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use oracle::stellar_rpc::{
    get_account_balance, get_latest_ledger_sequence, parse_latest_ledger_response,
};
use oracle::submit::{parse_get_transaction_response, parse_send_response};

const KEEPER: &str = "GDIEVMRSOQV3JKZ2CNUL2RQV4TTNAISKW4NAC25PQUQKGMWJO6DTOAE7";

const LATEST_LEDGER: &str = include_str!("fixtures/rpc/getLatestLedger.json");
const SEND_PENDING: &str = include_str!("fixtures/rpc/sendTransaction_PENDING.json");
const SEND_DUPLICATE: &str = include_str!("fixtures/rpc/sendTransaction_DUPLICATE.json");
const SEND_TRY_AGAIN: &str = include_str!("fixtures/rpc/sendTransaction_TRY_AGAIN_LATER.json");
const SEND_ERROR: &str = include_str!("fixtures/rpc/sendTransaction_ERROR_txBadSeq.json");
const TX_SUCCESS: &str = include_str!("fixtures/rpc/getTransaction_SUCCESS.json");
const TX_FAILED: &str = include_str!("fixtures/rpc/getTransaction_FAILED.json");
const TX_NOT_FOUND: &str = include_str!("fixtures/rpc/getTransaction_NOT_FOUND.json");
const HORIZON_ACCOUNT: &str = include_str!("fixtures/rpc/horizon_account.json");

#[test]
fn get_latest_ledger_fixture_parses() {
    assert_eq!(
        parse_latest_ledger_response(LATEST_LEDGER).unwrap(),
        2_539_605
    );
}

#[tokio::test]
async fn get_latest_ledger_sequence_reads_the_fixture_over_http() {
    let rpc = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LATEST_LEDGER))
        .mount(&rpc)
        .await;

    let sequence = get_latest_ledger_sequence(&rpc.uri()).await.unwrap();
    assert_eq!(sequence, 2_539_605);
}

#[test]
fn send_transaction_fixtures_parse_for_every_status() {
    for (body, status) in [
        (SEND_PENDING, "PENDING"),
        (SEND_DUPLICATE, "DUPLICATE"),
        (SEND_TRY_AGAIN, "TRY_AGAIN_LATER"),
        (SEND_ERROR, "ERROR"),
    ] {
        let result = parse_send_response(body).unwrap();
        assert_eq!(result.status, status);
        assert_eq!(result.hash.len(), 64, "{status}: hash is 32 bytes hex");
    }
}

#[test]
fn send_transaction_error_result_xdr_decodes_as_bad_sequence() {
    let result = parse_send_response(SEND_ERROR).unwrap();
    let xdr = result.error_result_xdr.expect("errorResultXdr");
    let Ok(decoded) = TransactionResult::from_xdr_base64(&xdr, Limits::none()) else {
        panic!("errorResultXdr is not a TransactionResult: {xdr}");
    };
    assert_eq!(decoded.fee_charged, 100);
    assert!(matches!(decoded.result, TransactionResultResult::TxBadSeq));
}

#[test]
fn get_transaction_fixtures_parse_for_every_status() {
    let success = parse_get_transaction_response(TX_SUCCESS).unwrap();
    assert_eq!(success.status, "SUCCESS");
    assert_eq!(success.ledger, Some(2_540_076));

    let failed = parse_get_transaction_response(TX_FAILED).unwrap();
    assert_eq!(failed.status, "FAILED");
    assert_eq!(failed.ledger, Some(2_540_077));

    let not_found = parse_get_transaction_response(TX_NOT_FOUND).unwrap();
    assert_eq!(not_found.status, "NOT_FOUND");
    assert_eq!(not_found.ledger, None);
}

#[tokio::test]
async fn horizon_account_fixture_yields_the_native_balance() {
    let horizon = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/accounts/{KEEPER}")))
        .respond_with(ResponseTemplate::new(200).set_body_string(HORIZON_ACCOUNT))
        .mount(&horizon)
        .await;

    let balance = get_account_balance(&horizon.uri(), KEEPER).await;
    assert_eq!(balance.unwrap().balance_stroops, 99_999_999_800);
}

/// Drift check against public testnet (#1423). Run on demand with
/// `cargo test -p oracle --test rpc_fixtures -- --ignored` to confirm the live
/// `getLatestLedger` response still parses the way the fixture does.
#[tokio::test]
#[ignore = "hits public testnet; run on demand (#1423)"]
async fn live_testnet_latest_ledger_still_parses() {
    let rpc_url = oracle::network_config::TESTNET_RPC_URL;
    let sequence = get_latest_ledger_sequence(rpc_url).await.unwrap();
    assert!(sequence > 0);
}
