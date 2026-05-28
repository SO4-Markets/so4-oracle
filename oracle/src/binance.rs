use serde::Deserialize;

pub const BINANCE_TICKER_PRICE_URL: &str = "https://api.binance.com/api/v3/ticker/price";
pub const FLOAT_PRECISION: i128 = 1_000_000_000_000_000_000_000_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinancePriceError {
    NetworkError(String),
    HttpError(u16),
    JsonError(String),
    PriceParseError(String),
}

#[derive(Debug, Deserialize)]
pub struct BinanceTickerEntry {
    pub symbol: String,
    pub price: String,
}

pub fn parse_ticker_response_body(
    body: &str,
    symbols: &[String],
) -> Result<Vec<(String, i128)>, BinancePriceError> {
    let entries: Vec<BinanceTickerEntry> = serde_json::from_str(body)
        .map_err(|err| BinancePriceError::JsonError(err.to_string()))?;

    let mut results = Vec::new();
    for symbol in symbols {
        let maybe = entries.iter().find(|entry| entry.symbol == *symbol);
        if let Some(found) = maybe {
            let scaled = parse_price_to_precision(&found.price)?;
            results.push((found.symbol.clone(), scaled));
        }
    }
    Ok(results)
}

pub fn parse_price_to_precision(raw: &str) -> Result<i128, BinancePriceError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(BinancePriceError::PriceParseError(
            "empty price string".to_string(),
        ));
    }
    if text.starts_with('-') {
        return Err(BinancePriceError::PriceParseError(
            "negative prices are not supported".to_string(),
        ));
    }

    let mut split = text.split('.');
    let whole = split.next().unwrap_or("0");
    let frac = split.next().unwrap_or("");
    if split.next().is_some() {
        return Err(BinancePriceError::PriceParseError(format!(
            "invalid decimal format: {text}"
        )));
    }

    let whole_val = whole
        .parse::<i128>()
        .map_err(|_| BinancePriceError::PriceParseError(format!("invalid whole part: {text}")))?;

    let scale_digits = 30usize;
    let normalized_frac = if frac.len() >= scale_digits {
        frac[..scale_digits].to_string()
    } else {
        let mut padded = frac.to_string();
        while padded.len() < scale_digits {
            padded.push('0');
        }
        padded
    };

    let frac_val = if normalized_frac.is_empty() {
        0
    } else {
        normalized_frac.parse::<i128>().map_err(|_| {
            BinancePriceError::PriceParseError(format!("invalid fractional part: {text}"))
        })?
    };

    let whole_scaled = whole_val
        .checked_mul(FLOAT_PRECISION)
        .ok_or_else(|| BinancePriceError::PriceParseError(format!("overflow for price: {text}")))?;
    whole_scaled
        .checked_add(frac_val)
        .ok_or_else(|| BinancePriceError::PriceParseError(format!("overflow for price: {text}")))
}

#[cfg(test)]
mod tests {
    use super::{parse_price_to_precision, parse_ticker_response_body, FLOAT_PRECISION};

    #[test]
    fn parse_price_integer() {
        assert_eq!(parse_price_to_precision("2").unwrap(), 2 * FLOAT_PRECISION);
    }

    #[test]
    fn parse_price_decimal() {
        assert_eq!(
            parse_price_to_precision("1.5").unwrap(),
            FLOAT_PRECISION + (FLOAT_PRECISION / 2)
        );
    }

    #[test]
    fn parse_price_invalid() {
        assert!(parse_price_to_precision("abc").is_err());
    }

    #[test]
    fn parse_ticker_response_filters_symbols() {
        let body = r#"[{"symbol":"BTCUSDT","price":"100.25"},{"symbol":"ETHUSDT","price":"10.5"}]"#;
        let symbols = vec!["ETHUSDT".to_string()];
        let parsed = parse_ticker_response_body(body, &symbols).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "ETHUSDT".to_string());
        assert_eq!(parsed[0].1, 10 * FLOAT_PRECISION + (FLOAT_PRECISION / 2));
    }
}
