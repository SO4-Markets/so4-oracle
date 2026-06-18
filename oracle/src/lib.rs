//! SO4 Oracle — library root.
//!
//! Shared types and module declarations for the oracle service.

#![allow(unused_must_use)]

pub mod binance;
pub mod coinbase;
pub mod config;
pub mod http;
pub mod keeper;
pub mod log;
pub mod network_config;
pub mod prices;
pub mod pyth;
pub mod retry;
pub mod signing;
pub mod stellar_rpc;
pub mod state;
pub mod submit;

pub use network_config::StellarNetwork;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

fn ser_i128_str<S: Serializer>(v: &i128, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&v.to_string())
}

fn de_i128_str<'de, D: Deserializer<'de>>(d: D) -> Result<i128, D::Error> {
    let raw = serde_json::Value::deserialize(d)?;
    match &raw {
        serde_json::Value::String(s) => s.parse::<i128>().map_err(serde::de::Error::custom),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(|v| v as i128)
            .or_else(|| n.as_u64().map(|v| v as i128))
            .ok_or_else(|| serde::de::Error::custom("i128 out of i64 range")),
        _ => Err(serde::de::Error::custom("expected string or number for i128")),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedPrice {
    pub token: String,
    pub symbol: String,
    #[serde(serialize_with = "ser_i128_str", deserialize_with = "de_i128_str")]
    pub min: i128,
    #[serde(serialize_with = "ser_i128_str", deserialize_with = "de_i128_str")]
    pub max: i128,
    pub timestamp: u64,
    pub ledger_seq: u32,
    pub sources_used: Vec<String>,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenPrice {
    pub token: String,
    pub symbol: String,
    pub price: i128,
    pub min: i128,
    pub max: i128,
    pub timestamp: u64,
    pub sources_used: Vec<String>,
    pub onchain_status: String,
    pub confirmed_ledger: Option<u32>,
    pub tx_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleStatus {
    pub last_submission_time: Option<u64>,
    pub last_onchain_submission_time: Option<u64>,
    pub last_cache_update_time: Option<u64>,
    pub network: String,
    pub keeper_balance_xlm: Option<f64>,
    pub tokens: Vec<TokenPrice>,
    pub recent_errors: Vec<String>,
    pub onchain_submission_supported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveOracleSnapshot {
    pub network: String,
    pub keeper_balance_xlm: Option<f64>,
    pub ledger_seq: Option<u32>,
    pub timestamp: u64,
    pub prices: Vec<CachedPrice>,
    pub recent_errors: Vec<String>,
    pub onchain_submission_supported: bool,
}
