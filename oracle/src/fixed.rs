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

    fn token_with_fixed_price(fixed_price: Option<&str>) -> TokenConfig {
        TokenConfig {
            symbol: "TUSDC".to_string(),
            display_symbol: Some("USDC".to_string()),
            stellar_address: "CADDR".to_string(),
            sources: vec!["fixed".to_string()],
            binance_symbol: None,
            coinbase_symbol: None,
            pyth_feed_id: None,
            fixed_price: fixed_price.map(|s| s.to_string()),
            min_sources: 1,
            max_deviation_bps: 100,
            stale_after_seconds: 60,
            submit_threshold_bps: 10,
            min: 0.0,
            max: 0.0,
            sources_used: vec![],
        }
    }

    #[test]
    fn parses_configured_fixed_price() {
        let token = token_with_fixed_price(Some("1000000000000000000000000000000"));
        assert_eq!(
            fixed_price(&token).unwrap(),
            1_000_000_000_000_000_000_000_000_000_000
        );
    }

    #[test]
    fn rejects_missing_fixed_price() {
        let token = token_with_fixed_price(None);
        assert_eq!(
            fixed_price(&token).unwrap_err(),
            FixedPriceError::MissingFixedPrice
        );
    }

    #[test]
    fn rejects_non_numeric_fixed_price() {
        let token = token_with_fixed_price(Some("not-a-number"));
        assert!(matches!(
            fixed_price(&token).unwrap_err(),
            FixedPriceError::InvalidFixedPrice(_)
        ));
    }

    #[test]
    fn rejects_zero_fixed_price() {
        let token = token_with_fixed_price(Some("0"));
        assert!(matches!(
            fixed_price(&token).unwrap_err(),
            FixedPriceError::InvalidFixedPrice(_)
        ));
    }

    #[test]
    fn rejects_negative_fixed_price() {
        let token = token_with_fixed_price(Some("-1000000000000000000000000000000"));
        assert!(matches!(
            fixed_price(&token).unwrap_err(),
            FixedPriceError::InvalidFixedPrice(_)
        ));
    }

    #[test]
    fn rejects_fixed_price_of_negative_one() {
        let token = token_with_fixed_price(Some("-1"));
        assert!(matches!(
            fixed_price(&token).unwrap_err(),
            FixedPriceError::InvalidFixedPrice(_)
        ));
    }

    #[test]
    fn accepts_smallest_valid_fixed_price() {
        let token = token_with_fixed_price(Some("1"));
        assert_eq!(fixed_price(&token).unwrap(), 1);
    }

    #[test]
    fn rejects_abc_string_with_invalid_fixed_price_error() {
        let token = token_with_fixed_price(Some("abc"));
        assert_eq!(
            fixed_price(&token).unwrap_err(),
            FixedPriceError::InvalidFixedPrice("abc".to_string()),
        );
    }

    // #370 — fixed_price parses the configured i128 string and returns it exactly
    #[test]
    fn issue_370_fixed_source_returns_configured_value() {
        // The oracle internally stores prices scaled to 30 decimal places.
        // A USDC-pegged token fixed at 1.0 would be configured as
        // "1000000000000000000000000000000" (1 followed by 30 zeros = 10^30).
        let configured = "1000000000000000000000000000000";
        let token = token_with_fixed_price(Some(configured));
        let result = fixed_price(&token).unwrap();
        assert_eq!(
            result,
            configured.parse::<i128>().unwrap(),
            "fixed_price must return the configured i128 value unchanged"
        );

        // Also verify a different concrete value parses correctly.
        let token2 = token_with_fixed_price(Some("42000000000000000000000000000000"));
        let result2 = fixed_price(&token2).unwrap();
        assert_eq!(result2, 42_000_000_000_000_000_000_000_000_000_000_i128);
    }
}
