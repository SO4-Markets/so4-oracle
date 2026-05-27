use soroban_sdk::{contracttype, Address};

// ---------------------------------------------------------------------------
// Liquidity (deposit / withdrawal) errors
// ---------------------------------------------------------------------------

/// Errors returned by the `liquidity_handler` contract.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum LiquidityError {
    /// A privileged function was called before `initialize`.
    NotInitialized = 10,
    /// Caller does not hold the required role.
    Unauthorized = 11,
    /// The market has not been registered with the liquidity handler.
    MarketNotRegistered = 12,
    /// Oracle prices have not been set for the market.
    PricesNotSet = 13,
    /// The account does not hold enough LP to cover the request.
    InsufficientLp = 14,
    /// Computed long output is below the caller's `min_long_out`.
    InsufficientLongOut = 15,
    /// Computed short output is below the caller's `min_short_out`.
    InsufficientShortOut = 16,
    /// No withdrawal record exists for the supplied id.
    WithdrawalNotFound = 17,
    /// A zero amount was supplied where a positive value is required.
    ZeroAmount = 18,
}

impl From<LiquidityError> for soroban_sdk::Error {
    fn from(e: LiquidityError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

/// The pool token pair backing a market, as registered with the liquidity
/// handler. These are the Soroban token contract addresses whose balances form
/// the market's long/short pools.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketTokens {
    pub long_token: Address,
    pub short_token: Address,
}

/// Oracle prices for a market's long/short tokens, scaled to a common
/// precision chosen by the price feeder. Used to value deposits when minting
/// LP shares.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OraclePrices {
    pub long_price: u128,
    pub short_price: u128,
}

/// A pending withdrawal awaiting execution. Created by `create_withdrawal`
/// (which escrows the caller's LP) and consumed by `execute_withdrawal`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Withdrawal {
    /// Account that owns the withdrawal (LP was debited from it).
    pub account: Address,
    pub market_id: u32,
    /// LP amount being redeemed.
    pub lp_amount: u128,
    /// Address that receives the withdrawn pool tokens.
    pub receiver: Address,
    /// Minimum acceptable long-token output (slippage guard).
    pub min_long_out: u128,
    /// Minimum acceptable short-token output (slippage guard).
    pub min_short_out: u128,
}

// ---------------------------------------------------------------------------
// Market errors
// ---------------------------------------------------------------------------

/// Errors returned by the `market_factory` contract.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MarketError {
    /// Caller does not hold the required role.
    Unauthorized = 1,
    /// The requested market does not exist.
    MarketNotFound = 2,
    /// The market is currently paused; the operation is not permitted.
    MarketPaused = 3,
    /// The market already exists and cannot be created again.
    MarketAlreadyExists = 4,
}

impl From<MarketError> for soroban_sdk::Error {
    fn from(e: MarketError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

// ---------------------------------------------------------------------------
// Market configuration
// ---------------------------------------------------------------------------

/// Optional configuration supplied when creating a new market.
///
/// All fields have sensible defaults when `None` is passed to `create_market`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketConfig {
    /// Maximum open interest allowed on the long side (u128 units).
    /// Defaults to `u128::MAX` when not provided.
    pub max_long_open_interest: u128,
    /// Maximum open interest allowed on the short side (u128 units).
    /// Defaults to `u128::MAX` when not provided.
    pub max_short_open_interest: u128,
}

// ---------------------------------------------------------------------------
// Market properties (on-chain record)
// ---------------------------------------------------------------------------

/// Full on-chain record for a created market.
///
/// Written to `data_store` at market creation time and updated as the market
/// lifecycle progresses.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketProps {
    /// Unique numeric identifier assigned at creation time.
    pub market_id: u32,
    /// The Soroban token contract address used for long positions.
    pub long_token: Address,
    /// The Soroban token contract address used for short positions.
    pub short_token: Address,
    /// The market LP / receipt token contract address.
    pub market_token: Address,
    /// Maximum open interest for the long side.
    pub max_long_open_interest: u128,
    /// Maximum open interest for the short side.
    pub max_short_open_interest: u128,
    /// Whether the market is currently paused.
    pub is_paused: bool,
}
