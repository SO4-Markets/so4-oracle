use soroban_sdk::{contracttype, Address, BytesN};

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum LiquidityError {
    NotInitialized = 10,
    Unauthorized = 11,
    MarketNotRegistered = 12,
    PricesNotSet = 13,
    InsufficientLp = 14,
    InsufficientLongOut = 15,
    InsufficientShortOut = 16,
    WithdrawalNotFound = 17,
    ZeroAmount = 18,
}

impl From<LiquidityError> for soroban_sdk::Error {
    fn from(e: LiquidityError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketTokens {
    pub long_token: Address,
    pub short_token: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OraclePrices {
    pub long_price: u128,
    pub short_price: u128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Withdrawal {
    pub account: Address,
    pub market_id: u32,
    pub lp_amount: u128,
    pub receiver: Address,
    pub min_long_out: u128,
    pub min_short_out: u128,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MarketError {
    Unauthorized = 1,
    MarketNotFound = 2,
    MarketPaused = 3,
    MarketAlreadyExists = 4,
}

impl From<MarketError> for soroban_sdk::Error {
    fn from(e: MarketError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketConfig {
    pub max_long_open_interest: u128,
    pub max_short_open_interest: u128,
    pub maintenance_margin_factor: u128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketProps {
    pub market_id: u32,
    pub long_token: Address,
    pub short_token: Address,
    pub market_token: Address,
    pub max_long_open_interest: u128,
    pub max_short_open_interest: u128,
    pub is_paused: bool,
    pub maintenance_margin_factor: u128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PositionProps {
    pub position_key: BytesN<32>,
    pub account: Address,
    pub market_id: u32,
    pub quantity: u128,
    pub collateral_amount: u128,
    pub average_price: u128,
    pub is_long: bool,
    pub is_open: bool,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PositionError {
    PositionNotFound = 30,
    MaxOpenInterestExceeded = 20,
    InsufficientCollateral = 21,
}

impl From<PositionError> for soroban_sdk::Error {
    fn from(e: PositionError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouterAction {
    SendTokens(Address, Address, u128),
    CreateDeposit(u32, u128, u128, Address),
    CancelDeposit(u32),
    CreateWithdrawal(u32, u128, Address, u128, u128),
    CancelWithdrawal(u32),
    CreateOrder(u32, i128, bool),
    UpdateOrder(u32, i128),
    CancelOrder(u32),
    ClaimFundingFees(u32, Address),
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum OrderError {
    UnsatisfiedTrigger = 40,
    OrderNotFound = 41,
    InsufficientOutput = 42,
    Unauthorized = 43,
    CannotUpdateMarketOrder = 44,
    OrderNotExpired = 45,
    InvalidOrderType = 46,
}

impl From<OrderError> for soroban_sdk::Error {
    fn from(e: OrderError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Position {
    pub account: Address,
    pub market_id: u32,
    pub is_long: bool,
    pub size_in_usd: u128,
    pub size_in_tokens: u128,
    pub collateral_amount: u128,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OrderType {
    MarketIncrease,
    MarketDecrease,
    LimitIncrease,
    LimitDecrease,
    StopIncrease,
    StopLossDecrease,
    LimitSwap,
    MarketSwap,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Order {
    pub key: u32,
    pub account: Address,
    pub market_id: u32,
    pub order_type: OrderType,
    pub is_long: bool,
    pub size_delta_usd: u128,
    pub collateral_delta: u128,
    pub trigger_price: u128,
    pub acceptable_price: u128,
    pub min_output_amount: u128,
    pub collateral_token: Address,
    pub amount_in: u128,
    pub created_at: u32,
    pub is_frozen: bool,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ConfigError {
    Unauthorized = 20,
    MarketNotFound = 21,
}

impl From<ConfigError> for soroban_sdk::Error {
    fn from(e: ConfigError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AdlError {
    AdlNotRequired = 50,
    NotProfitable = 51,
    Unauthorized = 52,
    PositionNotFound = 53,
}

impl From<AdlError> for soroban_sdk::Error {
    fn from(e: AdlError) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}
