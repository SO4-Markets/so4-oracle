use crate::state::{MarketSummary, Reader, ReaderError};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

use stellar_xdr::curr::{
    AccountId, BytesM, DecoratedSignature, Hash, HostFunction, Int128Parts, InvokeContractArgs,
    InvokeHostFunctionOp, Limits, Memo, MuxedAccount, Operation, OperationBody, Preconditions,
    PublicKey, ReadXdr, ScAddress, ScBytes, ScSymbol, ScVal, ScVec, SequenceNumber, Signature,
    SignatureHint, StringM, Transaction, TransactionEnvelope, TransactionExt,
    TransactionV1Envelope, UInt128Parts, Uint256, VecM, WriteXdr,
};

// ── Constants ──────────────────────────────────────────────────────────────

const DEFAULT_TESTNET_RPC: &str = "https://soroban-testnet.stellar.org";
/// Fixed-point precision used by the on-chain contracts (30 decimals).
const FLOAT_PRECISION: f64 = 1e30;
/// Limits for XDR serialisation — generous enough for any single view call.
const XDR_LIMITS: Limits = Limits {
    depth: 64,
    len: 1_048_576,
};

// ── RPC wire types ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct JsonRpcResp<T> {
    result: Option<T>,
    error: Option<JsonRpcErr>,
}

#[derive(Deserialize)]
struct JsonRpcErr {
    code: i64,
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SimResult {
    results: Option<Vec<SimInvokeResult>>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct SimInvokeResult {
    xdr: String,
}

// ── Soroban RPC client ────────────────────────────────────────────────────

/// Thin wrapper around the Soroban JSON-RPC endpoint that implements the
/// `Reader` trait by calling view functions on the deployed Reader contract
/// via `simulateTransaction`.
#[derive(Clone)]
pub struct RpcClient {
    http: reqwest::Client,
    rpc_url: String,
    /// Strkey (`C…`) of the deployed Reader contract.
    contract_id: String,
    /// Strkey (`C…`) of the deployed DataStore contract.
    datastore_id: String,
}

impl RpcClient {
    /// Build from environment variables.
    ///
    /// * `SOROBAN_RPC_URL` (falls back to `STELLAR_RPC_URL`, then testnet default)
    /// * `READER_CONTRACT_ID` (falls back to `ORACLE_CONTRACT_ID`)
    /// * `DATASTORE_CONTRACT_ID` (for account-position lookups)
    pub fn from_env() -> Self {
        let rpc_url = env::var("SOROBAN_RPC_URL")
            .or_else(|_| env::var("STELLAR_RPC_URL"))
            .unwrap_or_else(|_| DEFAULT_TESTNET_RPC.to_string());
        let contract_id = env::var("READER_CONTRACT_ID")
            .or_else(|_| env::var("ORACLE_CONTRACT_ID"))
            .unwrap_or_default();
        let datastore_id = env::var("DATASTORE_CONTRACT_ID").unwrap_or_default();

        Self {
            http: reqwest::Client::new(),
            rpc_url,
            contract_id,
            datastore_id,
        }
    }

    // ── helpers ────────────────────────────────────────────────────────────

    /// POST a JSON-RPC request and return the raw body.
    async fn rpc_post(&self, body: &serde_json::Value) -> Result<String, ReaderError> {
        let resp = self
            .http
            .post(&self.rpc_url)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                warn!("rpc POST failed: {e}");
                ReaderError::RpcError
            })?;

        if !resp.status().is_success() {
            warn!("rpc HTTP {}", resp.status());
            return Err(ReaderError::RpcError);
        }

        resp.text().await.map_err(|e| {
            warn!("rpc read body failed: {e}");
            ReaderError::RpcError
        })
    }

    /// Build a `ScAddress::Contract(…)` from a strkey string.
    fn contract_sc_address(strkey: &str) -> Result<ScAddress, ReaderError> {
        let sk = stellar_strkey::Strkey::from_string(strkey).map_err(|e| {
            warn!("bad strkey {strkey}: {e}");
            ReaderError::RpcError
        })?;
        match sk {
            stellar_strkey::Strkey::Contract(c) => Ok(ScAddress::Contract(Hash(c.0))),
            _ => {
                warn!("{strkey} is not a contract address");
                Err(ReaderError::RpcError)
            }
        }
    }

    /// Build a simulated transaction envelope XDR for a contract view call.
    fn build_sim_xdr(
        contract: &str,
        fn_name: &str,
        args: Vec<ScVal>,
    ) -> Result<String, ReaderError> {
        let contract_address = Self::contract_sc_address(contract)?;

        let fn_symbol = ScSymbol(
            StringM::try_from(fn_name.as_bytes().to_vec()).map_err(|e| {
                warn!("bad function name {fn_name}: {e:?}");
                ReaderError::RpcError
            })?,
        );

        let sc_args = ScVec(VecM::try_from(args).map_err(|e| {
            warn!("args conversion failed: {e:?}");
            ReaderError::RpcError
        })?);

        let invoke_args = InvokeContractArgs {
            contract_address,
            function_name: fn_symbol,
            args: sc_args.into(),
        };

        let op_body = OperationBody::InvokeHostFunction(InvokeHostFunctionOp {
            host_function: HostFunction::InvokeContract(invoke_args),
            auth: VecM::default(),
        });

        let op = Operation {
            source_account: None,
            body: op_body,
        };

        let source_account = MuxedAccount::Ed25519(Uint256([0u8; 32]));

        let ops = VecM::try_from(vec![op]).map_err(|e| {
            warn!("ops conversion failed: {e:?}");
            ReaderError::RpcError
        })?;

        let tx = Transaction {
            source_account,
            fee: 0,
            seq_num: SequenceNumber(0),
            cond: Preconditions::None,
            memo: Memo::None,
            operations: ops,
            ext: TransactionExt::V0,
        };

        let sigs = VecM::try_from(vec![DecoratedSignature {
            hint: SignatureHint([0u8; 4]),
            signature: Signature(BytesM::try_from(vec![0u8; 64]).map_err(|e| {
                warn!("sig conversion failed: {e:?}");
                ReaderError::RpcError
            })?),
        }])
        .map_err(|e| {
            warn!("sigs conversion failed: {e:?}");
            ReaderError::RpcError
        })?;

        let envelope = TransactionEnvelope::Tx(TransactionV1Envelope { tx, signatures: sigs });

        envelope.to_xdr_base64(XDR_LIMITS).map_err(|e| {
            warn!("xdr encode failed: {e}");
            ReaderError::RpcError
        })
    }

    /// Simulate a view call on an arbitrary contract.
    async fn simulate_on(
        &self,
        contract_id: &str,
        fn_name: &str,
        args: Vec<ScVal>,
    ) -> Result<ScVal, ReaderError> {
        let xdr_b64 = Self::build_sim_xdr(contract_id, fn_name, args)?;

        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "simulateTransaction",
            "params": [{ "transaction": xdr_b64 }]
        });

        Self::parse_sim_response(self.rpc_post(&body).await?)
    }

    /// Simulate a contract view function on the Reader contract.
    async fn simulate(
        &self,
        fn_name: &str,
        args: Vec<ScVal>,
    ) -> Result<ScVal, ReaderError> {
        let xdr_b64 = Self::build_sim_xdr(&self.contract_id, fn_name, args)?;

        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "simulateTransaction",
            "params": [{ "transaction": xdr_b64 }]
        });

        Self::parse_sim_response(self.rpc_post(&body).await?)
    }

    /// Parse the JSON-RPC simulateTransaction response.
    fn parse_sim_response(raw: String) -> Result<ScVal, ReaderError> {
        let resp: JsonRpcResp<SimResult> = serde_json::from_str(&raw).map_err(|e| {
            warn!("rpc json parse failed: {e}");
            ReaderError::RpcError
        })?;

        if let Some(err) = resp.error {
            warn!("rpc error {}: {}", err.code, err.message);
            return Err(ReaderError::RpcError);
        }

        let result = resp.result.ok_or_else(|| {
            warn!("rpc missing result");
            ReaderError::RpcError
        })?;

        if let Some(err) = result.error {
            warn!("simulate error: {err}");
            return Err(ReaderError::RpcError);
        }

        let invokes = result.results.ok_or_else(|| {
            warn!("simulate missing results array");
            ReaderError::RpcError
        })?;

        let first = invokes.first().ok_or_else(|| {
            warn!("simulate empty results");
            ReaderError::RpcError
        })?;

        ScVal::from_xdr_base64(&first.xdr, XDR_LIMITS).map_err(|e| {
            warn!("result xdr decode failed: {e}");
            ReaderError::RpcError
        })
    }

    // ── ScVal extraction helpers ───────────────────────────────────────────

    fn as_u128(v: &ScVal) -> Option<u128> {
        match v {
            ScVal::U128(UInt128Parts { hi, lo }) => Some(((*hi as u128) << 64) | (*lo as u128)),
            _ => None,
        }
    }

    fn as_i128(v: &ScVal) -> Option<i128> {
        match v {
            ScVal::I128(Int128Parts { hi, lo }) => Some(((*hi as i128) << 64) | (*lo as u128 as i128)),
            _ => None,
        }
    }

    fn as_u32(v: &ScVal) -> Option<u32> {
        match v {
            ScVal::U32(n) => Some(*n),
            _ => None,
        }
    }

    fn as_bool(v: &ScVal) -> Option<bool> {
        match v {
            ScVal::Bool(b) => Some(*b),
            _ => None,
        }
    }

    fn as_bytes(v: &ScVal) -> Option<Vec<u8>> {
        match v {
            ScVal::Bytes(b) => Some(b.0.to_vec()),
            _ => None,
        }
    }

    fn as_vec(v: &ScVal) -> Option<Vec<ScVal>> {
        match v {
            ScVal::Vec(Some(v)) => Some(v.to_vec()),
            _ => None,
        }
    }

    /// Scale a u128 fixed-point value (30 decimals) to f64.
    fn to_f64(v: u128) -> f64 {
        (v as f64) / FLOAT_PRECISION
    }

    fn to_f64_signed(v: i128) -> f64 {
        (v as f64) / FLOAT_PRECISION
    }

    /// Build an `ScVal::Address(ScAddress::Account(…))` from a Stellar
    /// public-key strkey (`G…`).
    fn account_sc_val(strkey: &str) -> Result<ScVal, ReaderError> {
        let sk = stellar_strkey::Strkey::from_string(strkey).map_err(|e| {
            warn!("bad account strkey {strkey}: {e}");
            ReaderError::NotFound
        })?;
        match sk {
            stellar_strkey::Strkey::PublicKeyEd25519(pk) => Ok(ScVal::Address(
                ScAddress::Account(AccountId(PublicKey::PublicKeyTypeEd25519(Uint256(pk.0)))),
            )),
            _ => {
                warn!("{strkey} is not a public key address");
                Err(ReaderError::NotFound)
            }
        }
    }
}

// ── Retry helper ───────────────────────────────────────────────────────────

async fn retry<T, F, Fut>(mut f: F) -> Result<T, ReaderError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ReaderError>>,
{
    let mut backoff = 50u64;
    for attempt in 0..3u32 {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt == 2 {
                    return Err(e);
                }
                sleep(Duration::from_millis(backoff)).await;
                backoff *= 2;
                if backoff > 400 {
                    return Err(e);
                }
            }
        }
    }
    unreachable!()
}

// ── Reader implementation ──────────────────────────────────────────────────

#[async_trait]
impl Reader for RpcClient {
    /// Return the list of market IDs.
    ///
    /// The Reader contract does not expose a "list markets" function, so we
    /// read the set of configured market IDs from the `MARKET_IDS` env var
    /// (comma-separated u32 values, e.g. "0,1,2").
    async fn get_markets(&self) -> Result<Vec<String>, ReaderError> {
        let ids = env::var("MARKET_IDS").unwrap_or_default();
        if ids.trim().is_empty() {
            warn!("MARKET_IDS is not set — returning empty market list");
            return Ok(Vec::new());
        }
        Ok(ids
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }

    /// Call Reader::get_market_pool_value_info(market_id, long_price,
    /// short_price, maximize) and return a `MarketSummary`.
    async fn get_market_pool_value_info(
        &self,
        market: &str,
    ) -> Result<MarketSummary, ReaderError> {
        let client = self.clone();
        let mkt = market.to_string();
        retry(|| {
            let client = client.clone();
            let mkt = mkt.clone();
            async move {
                let market_id: u32 = mkt.parse().map_err(|_| {
                    warn!("bad market id: {mkt}");
                    ReaderError::NotFound
                })?;

                // Use max u128 for both prices and maximize=false.
                let result = client
                    .simulate(
                        "get_market_pool_value_info",
                        vec![
                            ScVal::U32(market_id),
                            ScVal::U128(UInt128Parts {
                                hi: u64::MAX,
                                lo: u64::MAX,
                            }),
                            ScVal::U128(UInt128Parts {
                                hi: u64::MAX,
                                lo: u64::MAX,
                            }),
                            ScVal::Bool(false),
                        ],
                    )
                    .await?;

                // PoolValueInfo returned as Vec: [pool_value(u128), long_pnl(i128),
                // short_pnl(i128), impact_pool_amount(u128), net_pnl(i128),
                // lp_supply(u128), index_token_price(u128)]
                let fields = Self::as_vec(&result).ok_or_else(|| {
                    warn!("unexpected pool value return type");
                    ReaderError::RpcError
                })?;

                if fields.len() < 7 {
                    warn!("pool value info too short: {} fields", fields.len());
                    return Err(ReaderError::RpcError);
                }

                let pool_value = Self::as_u128(&fields[0]).unwrap_or(0);
                let long_oi = Self::as_u128(&fields[1]).unwrap_or(0);
                let short_oi = Self::as_u128(&fields[2]).unwrap_or(0);
                let _index_token_price = Self::as_u128(&fields[6]).unwrap_or(0);

                // Fetch funding info for this market
                let funding_factor = match client
                    .simulate("get_funding_info", vec![ScVal::U32(market_id)])
                    .await
                {
                    Ok(fv) => {
                        let ff = Self::as_vec(&fv).unwrap_or_default();
                        if ff.len() >= 1 {
                            Self::as_i128(&ff[0]).unwrap_or(0)
                        } else {
                            0
                        }
                    }
                    Err(_) => 0,
                };

                Ok(MarketSummary {
                    market_token_address: String::new(),
                    index_token: String::new(),
                    long_token: String::new(),
                    short_token: String::new(),
                    pool_value_usd: Self::to_f64(pool_value),
                    long_oi: Self::to_f64(long_oi),
                    short_oi: Self::to_f64(short_oi),
                    current_funding_rate: Self::to_f64_signed(funding_factor),
                })
            }
        })
        .await
    }

    /// Return a JSON object with market detail including funding info and OI.
    async fn get_market_detail(&self, market: &str) -> Result<serde_json::Value, ReaderError> {
        let client = self.clone();
        let mkt = market.to_string();
        retry(|| {
            let client = client.clone();
            let mkt = mkt.clone();
            async move {
                let market_id: u32 = mkt.parse().map_err(|_| {
                    warn!("bad market id: {mkt}");
                    ReaderError::NotFound
                })?;

                let funding = client
                    .simulate("get_funding_info", vec![ScVal::U32(market_id)])
                    .await;
                let funding_fields = funding
                    .ok()
                    .and_then(|v| Self::as_vec(&v))
                    .unwrap_or_default();

                let oi = client
                    .simulate("get_open_interest", vec![ScVal::U32(market_id)])
                    .await;
                let oi_fields = oi
                    .ok()
                    .and_then(|v| Self::as_vec(&v))
                    .unwrap_or_default();

                let long_oi = if oi_fields.len() >= 1 {
                    Self::as_u128(&oi_fields[0]).unwrap_or(0)
                } else if funding_fields.len() >= 2 {
                    Self::as_u128(&funding_fields[1]).unwrap_or(0)
                } else {
                    0
                };
                let short_oi = if oi_fields.len() >= 2 {
                    Self::as_u128(&oi_fields[1]).unwrap_or(0)
                } else if funding_fields.len() >= 3 {
                    Self::as_u128(&funding_fields[2]).unwrap_or(0)
                } else {
                    0
                };

                let funding_factor = if funding_fields.len() >= 1 {
                    Self::as_i128(&funding_fields[0]).unwrap_or(0)
                } else {
                    0
                };

                Ok(json!({
                    "market_id": market_id,
                    "long_oi": Self::to_f64(long_oi),
                    "short_oi": Self::to_f64(short_oi),
                    "funding_factor_per_second": Self::to_f64_signed(funding_factor),
                    "top_positions": [],
                }))
            }
        })
        .await
    }

    /// Call DataStore::get_account_positions(account, 0, u32::MAX) via
    /// simulateTransaction and return the list of position key hex strings.
    async fn get_account_positions(&self, account: &str) -> Result<Vec<String>, ReaderError> {
        let client = self.clone();
        let acct = account.to_string();
        retry(|| {
            let client = client.clone();
            let acct = acct.clone();
            async move {
                let sc_addr = Self::account_sc_val(&acct)?;

                if client.datastore_id.is_empty() {
                    warn!("DATASTORE_CONTRACT_ID is not set — cannot list positions");
                    return Ok(Vec::new());
                }

                let result = client
                    .simulate_on(
                        &client.datastore_id,
                        "get_account_positions",
                        vec![
                            sc_addr,
                            ScVal::U32(0),
                            ScVal::U32(u32::MAX),
                        ],
                    )
                    .await?;

                let entries = Self::as_vec(&result).ok_or_else(|| {
                    warn!("unexpected account positions return type");
                    ReaderError::RpcError
                })?;

                let mut keys = Vec::new();
                for entry in entries.iter() {
                    // PositionProps: [position_key, account, market_id, quantity,
                    //   collateral_amount, average_price, is_long, is_open, referral_code]
                    if let Some(fields) = Self::as_vec(entry) {
                        if let Some(key_bytes) = fields.first().and_then(|k| Self::as_bytes(k)) {
                            keys.push(hex::encode(key_bytes));
                        }
                    }
                }

                Ok(keys)
            }
        })
        .await
    }

    /// Call Reader::get_position_info(position_key, maximize) and return the
    /// result as a JSON Value.
    async fn get_position_info(&self, position_id: &str) -> Result<serde_json::Value, ReaderError> {
        let client = self.clone();
        let pid = position_id.to_string();
        retry(|| {
            let client = client.clone();
            let pid = pid.clone();
            async move {
                let key_bytes = hex::decode(&pid).map_err(|e| {
                    warn!("bad position key hex {pid}: {e}");
                    ReaderError::NotFound
                })?;
                if key_bytes.len() != 32 {
                    warn!("position key must be 32 bytes, got {}", key_bytes.len());
                    return Err(ReaderError::NotFound);
                }

                let sc_bytes =
                    ScVal::Bytes(ScBytes(BytesM::try_from(key_bytes).map_err(|e| {
                        warn!("bytes conversion failed: {e:?}");
                        ReaderError::RpcError
                    })?));

                let result = client
                    .simulate(
                        "get_position_info",
                        vec![sc_bytes, ScVal::Bool(false)],
                    )
                    .await?;

                // PositionInfo: [position(Vec), pnl_usd(i128), pending_fees(Vec),
                //                liquidation_price(u128), funding_info(Vec)]
                let fields = Self::as_vec(&result).ok_or_else(|| {
                    warn!("unexpected position info return type");
                    ReaderError::RpcError
                })?;

                if fields.len() < 5 {
                    warn!("position info too short: {} fields", fields.len());
                    return Err(ReaderError::RpcError);
                }

                let pos_fields = Self::as_vec(&fields[0]).unwrap_or_default();
                let pos_market_id = pos_fields.get(2).and_then(|v| Self::as_u32(v)).unwrap_or(0);
                let pos_is_long = pos_fields.get(6).and_then(|v| Self::as_bool(v)).unwrap_or(false);
                let pos_quantity = pos_fields.get(3).and_then(|v| Self::as_u128(v)).unwrap_or(0);
                let pos_collateral = pos_fields.get(4).and_then(|v| Self::as_u128(v)).unwrap_or(0);
                let pos_avg_price = pos_fields.get(5).and_then(|v| Self::as_u128(v)).unwrap_or(0);

                let pnl_usd = Self::as_i128(&fields[1]).unwrap_or(0);
                let liq_price = Self::as_u128(&fields[3]).unwrap_or(0);

                Ok(json!({
                    "position_key": pid,
                    "market_id": pos_market_id,
                    "is_long": pos_is_long,
                    "quantity": Self::to_f64(pos_quantity),
                    "collateral": Self::to_f64(pos_collateral),
                    "entry_price": Self::to_f64(pos_avg_price),
                    "pnl_usd": Self::to_f64_signed(pnl_usd),
                    "liquidation_price": Self::to_f64(liq_price),
                }))
            }
        })
        .await
    }

    /// Get the latest index-token price by calling
    /// Reader::get_market_pool_value_info and extracting `index_token_price`.
    async fn get_latest_price(&self, token: &str) -> Result<f64, ReaderError> {
        let client = self.clone();
        let tok = token.to_string();
        retry(|| {
            let client = client.clone();
            let tok = tok.clone();
            async move {
                let market_id: u32 = tok.parse().map_err(|_| {
                    warn!("get_latest_price: cannot parse {tok} as market id");
                    ReaderError::NotFound
                })?;

                let result = client
                    .simulate(
                        "get_market_pool_value_info",
                        vec![
                            ScVal::U32(market_id),
                            ScVal::U128(UInt128Parts {
                                hi: u64::MAX,
                                lo: u64::MAX,
                            }),
                            ScVal::U128(UInt128Parts {
                                hi: u64::MAX,
                                lo: u64::MAX,
                            }),
                            ScVal::Bool(false),
                        ],
                    )
                    .await?;

                let fields = Self::as_vec(&result).ok_or_else(|| {
                    warn!("unexpected pool value return type for price");
                    ReaderError::RpcError
                })?;

                let price = fields
                    .get(6)
                    .and_then(|v| Self::as_u128(v))
                    .ok_or_else(|| {
                        warn!("missing index_token_price field");
                        ReaderError::RpcError
                    })?;

                Ok(Self::to_f64(price))
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ScVal extraction helpers ───────────────────────────────────────────

    #[test]
    fn as_u128_extracts_value() {
        let val = ScVal::U128(UInt128Parts { hi: 0, lo: 42 });
        assert_eq!(RpcClient::as_u128(&val), Some(42));
    }

    #[test]
    fn as_u128_large_value() {
        let val = ScVal::U128(UInt128Parts {
            hi: 1,
            lo: 0,
        });
        assert_eq!(RpcClient::as_u128(&val), Some(1u128 << 64));
    }

    #[test]
    fn as_u128_none_for_wrong_type() {
        let val = ScVal::U32(42);
        assert_eq!(RpcClient::as_u128(&val), None);
    }

    #[test]
    fn as_i128_extracts_positive() {
        let val = ScVal::I128(Int128Parts { hi: 0, lo: 100 });
        assert_eq!(RpcClient::as_i128(&val), Some(100));
    }

    #[test]
    fn as_i128_extracts_negative() {
        let val = ScVal::I128(Int128Parts {
            hi: -1i64,
            lo: u64::MAX,
        });
        assert_eq!(RpcClient::as_i128(&val), Some(-1));
    }

    #[test]
    fn as_u32_extracts_value() {
        let val = ScVal::U32(99);
        assert_eq!(RpcClient::as_u32(&val), Some(99));
    }

    #[test]
    fn as_bool_extracts_value() {
        assert_eq!(RpcClient::as_bool(&ScVal::Bool(true)), Some(true));
        assert_eq!(RpcClient::as_bool(&ScVal::Bool(false)), Some(false));
        assert_eq!(RpcClient::as_bool(&ScVal::U32(0)), None);
    }

    #[test]
    fn as_bytes_extracts_value() {
        let bytes = vec![1u8, 2, 3];
        let sc_bytes = ScBytes(BytesM::try_from(bytes.clone()).unwrap());
        let val = ScVal::Bytes(sc_bytes);
        assert_eq!(RpcClient::as_bytes(&val), Some(bytes));
    }

    #[test]
    fn as_vec_extracts_values() {
        let vals = vec![ScVal::U32(1), ScVal::U32(2)];
        let sc_vec = ScVal::Vec(Some(ScVec(VecM::try_from(vals).unwrap())));
        let extracted = RpcClient::as_vec(&sc_vec).unwrap();
        assert_eq!(extracted.len(), 2);
    }

    #[test]
    fn as_vec_none_for_non_vec() {
        assert!(RpcClient::as_vec(&ScVal::U32(0)).is_none());
    }

    // ── Fixed-point conversion ─────────────────────────────────────────────

    #[test]
    fn to_f64_converts_correctly() {
        let one_usd: u128 = 1_000_000_000_000_000_000_000_000_000_000; // 1e30
        assert!((RpcClient::to_f64(one_usd) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn to_f64_signed_converts_positive() {
        let val: i128 = 1_000_000_000_000_000_000_000_000_000_000;
        assert!((RpcClient::to_f64_signed(val) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn to_f64_signed_converts_negative() {
        let val: i128 = -500_000_000_000_000_000_000_000_000_000;
        assert!((RpcClient::to_f64_signed(val) - (-0.5)).abs() < 1e-10);
    }

    // ── XDR envelope building ──────────────────────────────────────────────

    #[test]
    fn build_sim_xdr_produces_valid_base64() {
        let result = RpcClient::build_sim_xdr(
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
            "get_funding_info",
            vec![ScVal::U32(0)],
        );
        assert!(result.is_ok(), "build_sim_xdr failed: {:?}", result.err());
        let b64 = result.unwrap();
        // Should be valid base64
        assert!(!b64.is_empty());
        // Should be decodable as XDR
        let envelope = TransactionEnvelope::from_xdr_base64(&b64, XDR_LIMITS);
        assert!(envelope.is_ok(), "XDR decode failed: {:?}", envelope.err());
    }

    #[test]
    fn build_sim_xdr_with_multiple_args() {
        let result = RpcClient::build_sim_xdr(
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
            "get_market_pool_value_info",
            vec![
                ScVal::U32(0),
                ScVal::U128(UInt128Parts { hi: 0, lo: 100 }),
                ScVal::U128(UInt128Parts { hi: 0, lo: 200 }),
                ScVal::Bool(false),
            ],
        );
        assert!(result.is_ok(), "build_sim_xdr failed: {:?}", result.err());
    }

    #[test]
    fn build_sim_xdr_rejects_bad_contract() {
        let result = RpcClient::build_sim_xdr("not-a-strkey", "foo", vec![]);
        assert!(result.is_err());
    }

    // ── Simulate response parsing ──────────────────────────────────────────

    #[test]
    fn parse_sim_response_success() {
        // Build a valid ScVal and encode to XDR base64
        let sc_val = ScVal::U32(42);
        let xdr_b64 = sc_val.to_xdr_base64(XDR_LIMITS).unwrap();

        let resp_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "results": [{ "xdr": xdr_b64 }],
                "latestLedger": 100
            }
        })
        .to_string();

        let result = RpcClient::parse_sim_response(resp_body);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ScVal::U32(42));
    }

    #[test]
    fn parse_sim_response_rpc_error() {
        let resp_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32600, "message": "Invalid request" }
        })
        .to_string();

        let result = RpcClient::parse_sim_response(resp_body);
        assert!(result.is_err());
    }

    #[test]
    fn parse_sim_response_sim_error() {
        let resp_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "error": "contract error",
                "latestLedger": 100
            }
        })
        .to_string();

        let result = RpcClient::parse_sim_response(resp_body);
        assert!(result.is_err());
    }

    #[test]
    fn parse_sim_response_empty_results() {
        let resp_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "results": [],
                "latestLedger": 100
            }
        })
        .to_string();

        let result = RpcClient::parse_sim_response(resp_body);
        assert!(result.is_err());
    }

    #[test]
    fn parse_sim_response_malformed_json() {
        let result = RpcClient::parse_sim_response("not json".to_string());
        assert!(result.is_err());
    }

    // ── Account ScVal construction ─────────────────────────────────────────

    #[test]
    fn account_sc_val_from_valid_strkey() {
        // Use the well-known test account
        let result =
            RpcClient::account_sc_val("GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF");
        assert!(result.is_ok());
        match result.unwrap() {
            ScVal::Address(ScAddress::Account(_)) => {}
            other => panic!("expected ScVal::Address(Account), got {:?}", other),
        }
    }

    #[test]
    fn account_sc_val_rejects_contract_strkey() {
        let result =
            RpcClient::account_sc_val("CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4");
        assert!(result.is_err());
    }

    #[test]
    fn account_sc_val_rejects_invalid() {
        assert!(RpcClient::account_sc_val("not-valid").is_err());
    }

    // ── Retry helper ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn retry_succeeds_on_first_try() {
        let result = retry(|| async { Ok::<_, ReaderError>(42) }).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn retry_retries_on_failure() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let attempts_clone = attempts.clone();
        let result = retry(|| {
            let a = attempts_clone.clone();
            async move {
                let n = a.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n < 2 {
                    Err(ReaderError::RpcError)
                } else {
                    Ok(42)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_fails_after_max_attempts() {
        let result = retry(|| async { Err::<i32, _>(ReaderError::RpcError) }).await;
        assert!(result.is_err());
    }
}
