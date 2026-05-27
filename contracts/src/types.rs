use soroban_sdk::{contracttype, Address, BytesN};

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
    /// Maintenance margin factor, scaled by 1,000,000.
    /// E.g., 50,000 = 5%.
    pub maintenance_margin_factor: u128,
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
    /// Maintenance margin factor.
    pub maintenance_margin_factor: u128,
}

/// On-chain record for a position owned by an account.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PositionProps {
    /// Unique identifier for the position entry.
    pub position_key: BytesN<32>,
    /// Account that owns the position.
    pub account: Address,
    /// Market ID associated with the position.
    pub market_id: u32,
    /// Position quantity / notional value.
    pub quantity: u128,
    /// Collateral amount held for the position.
    pub collateral_amount: u128,
    /// Average entry price.
    pub average_price: u128,
    /// Whether the position is long.
    pub is_long: bool,
    /// Whether the position remains open.
    pub is_open: bool,
}

/// Errors returned by the `position_handler` contract and position utility
/// functions.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PositionError {
    /// The requested position does not exist.
    PositionNotFound = 30,
    /// The resulting position size would exceed the market's max open interest.
    MaxOpenInterestExceeded = 20,
    /// The remaining collateral after a partial close is below the minimum
    /// collateral factor.
    InsufficientCollateral = 21,
}

impl From<PositionError> for soroban_sdk::Error {
    fn from(e: PositionError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

// ---------------------------------------------------------------------------
// Router actions (issue #22)
// ---------------------------------------------------------------------------

/// A discrete action executed by the `Router`'s `multicall` loop.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouterAction {
    /// Transfer tokens from the caller to a receiver: (token, receiver, amount).
    SendTokens(Address, Address, u128),

    /// Create a deposit request: (market_id, long_amount, short_amount, receiver).
    CreateDeposit(u32, u128, u128, Address),

    /// Placeholder for cancelling a pending deposit: (deposit_id).
    CancelDeposit(u32),

    /// Create a pending withdrawal: (market_id, lp_amount, receiver, min_long_out, min_short_out).
    CreateWithdrawal(u32, u128, Address, u128, u128),

    /// Placeholder for cancelling a pending withdrawal: (withdrawal_id).
    CancelWithdrawal(u32),

    /// Placeholder for creating a new order: (market_id, size_delta, is_long).
    CreateOrder(u32, i128, bool),

    /// Placeholder for updating an existing order: (order_id, size_delta).
    UpdateOrder(u32, i128),

    /// Placeholder for cancelling a pending order: (order_id).
    CancelOrder(u32),

    /// Placeholder for claiming accrued funding fees: (market_id, receiver).
    ClaimFundingFees(u32, Address),
}

// ---------------------------------------------------------------------------
// Order / position types (issues #43, #44, #45, #46)
// ---------------------------------------------------------------------------

/// Errors returned by order handler functions.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum OrderError {
    /// The trigger price condition for the order is not yet satisfied.
    UnsatisfiedTrigger = 40,
    /// The order does not exist.
    OrderNotFound = 41,
    /// The swap output is below the caller-specified minimum.
    InsufficientOutput = 42,
    /// The execution price is outside the order's acceptable price bound.
    UnacceptablePrice = 43,
}

impl From<OrderError> for soroban_sdk::Error {
    fn from(e: OrderError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

/// A lightweight position used by the position utility functions.
///
/// Distinct from [`PositionProps`] (which is the full on-chain record stored
/// by the position_handler). This struct is used as a mutable working copy
/// inside `increase_position_utils` and `decrease_position_utils`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Position {
    /// Account that owns the position.
    pub account: Address,
    /// Market this position belongs to.
    pub market_id: u32,
    /// Whether this is a long (true) or short (false) position.
    pub is_long: bool,
    /// Notional size of the position in USD (scaled integer).
    pub size_in_usd: u128,
    /// Size expressed in index tokens.
    pub size_in_tokens: u128,
    /// Collateral amount deposited (in collateral token units).
    pub collateral_amount: u128,
}

/// The type of an order.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OrderType {
    /// Open or increase a position at the current market price.
    MarketIncrease,
    /// Close or decrease a position at the current market price.
    MarketDecrease,
    /// Open or increase a position when price drops to / below trigger_price
    /// (for longs) or rises to / above trigger_price (for shorts).
    LimitIncrease,
    /// Close or decrease a position when price drops to / below trigger_price.
    StopLossDecrease,
    /// Swap tokens when the execution price satisfies the trigger condition.
    /// For a sell swap: executes when `price <= trigger_price`.
    /// For a buy swap:  executes when `price >= trigger_price`.
    LimitSwap,
    /// Swap tokens immediately at the current market price along a given path.
    MarketSwap,
}

/// A pending order.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Order {
    /// Account that placed the order.
    pub account: Address,
    pub market_id: u32,
    pub order_type: OrderType,
    pub is_long: bool,
    /// USD size delta for the order.
    pub size_delta_usd: u128,
    /// Collateral delta (used for increase orders).
    pub collateral_delta: u128,
    /// Trigger price (0 for market orders).
    pub trigger_price: u128,
}

// ---------------------------------------------------------------------------
// Config handler errors
// ---------------------------------------------------------------------------

/// Errors returned by the `config_handler` contract.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ConfigError {
    /// Caller does not hold the required role.
    Unauthorized = 20,
    /// The requested market does not exist.
    MarketNotFound = 21,
}

impl From<ConfigError> for soroban_sdk::Error {
    fn from(e: ConfigError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}
