use crate::cache::Cache;
use crate::history::HistoryStore;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

#[derive(Clone)]
pub struct AppState {
    pub cache: Cache,
    pub reader: Arc<dyn Reader + Send + Sync>,
    pub history: HistoryStore,
}

#[derive(Error, Debug)]
pub enum ReaderError {
    #[error("not found")]
    NotFound,
    #[error("rpc error")]
    RpcError,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MarketSummary {
    pub market_token_address: String,
    pub index_token: String,
    pub long_token: String,
    pub short_token: String,
    pub pool_value_usd: f64,
    pub long_oi: f64,
    pub short_oi: f64,
    pub current_funding_rate: f64,
}

#[async_trait]
pub trait Reader {
    async fn get_markets(&self) -> Result<Vec<String>, ReaderError>;
    async fn get_market_pool_value_info(&self, market: &str) -> Result<MarketSummary, ReaderError>;
    async fn get_market_detail(&self, market: &str) -> Result<serde_json::Value, ReaderError>;
    async fn get_account_positions(&self, account: &str) -> Result<Vec<String>, ReaderError>;
    async fn get_position_info(&self, position_id: &str) -> Result<serde_json::Value, ReaderError>;
    async fn get_latest_price(&self, token: &str) -> Result<f64, ReaderError>;
}
