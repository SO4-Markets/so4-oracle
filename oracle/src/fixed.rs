use shared_config::TokenConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixedPriceError {
    MissingFixedPrice,
    InvalidFixedPrice(String),
}

pub fn fixed_price(token: &TokenConfig) -> Result<i128, FixedPriceError> {
    let raw = token
        .fixed_price
        .as_deref()
        .ok_or(FixedPriceError::MissingFixedPrice)?;
    let price = raw
        .parse::<i128>()
        .map_err(|_| FixedPriceError::InvalidFixedPrice(raw.to_string()))?;
    if price <= 0 {
        return Err(FixedPriceError::InvalidFixedPrice(raw.to_string()));
    }
    Ok(price)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_configured_fixed_price() {
        let token = TokenConfig {
            symbol: "TUSDC".to_string(),
            display_symbol: Some("USDC".to_string()),
            stellar_address: "CADDR".to_string(),
            sources: vec!["fixed".to_string()],
            binance_symbol: None,
            coinbase_symbol: None,
            pyth_feed_id: None,
            fixed_price: Some("1000000000000000000000000000000".to_string()),
            min_sources: 1,
            max_deviation_bps: 100,
            stale_after_seconds: 60,
            submit_threshold_bps: 10,
            min: 0.0,
            max: 0.0,
            sources_used: vec![],
        };

        assert_eq!(
            fixed_price(&token).unwrap(),
            1_000_000_000_000_000_000_000_000_000_000
        );
    }
}
