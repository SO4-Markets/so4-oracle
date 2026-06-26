use serde::Deserialize;

pub const BINANCE_TICKER_PRICE_URL: &str = "https://data-api.binance.vision/api/v3/ticker/price";
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

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BinanceTickerResponse {
    Single(BinanceTickerEntry),
    Many(Vec<BinanceTickerEntry>),
}

pub fn parse_ticker_response_body(
    body: &str,
    symbols: &[String],
) -> Result<Vec<(String, i128)>, BinancePriceError> {
    let response: BinanceTickerResponse =
        serde_json::from_str(body).map_err(|err| BinancePriceError::JsonError(err.to_string()))?;
    let entries = match response {
        BinanceTickerResponse::Single(entry) => vec![entry],
        BinanceTickerResponse::Many(entries) => entries,
    };

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

pub fn parse_ticker_http_response(
    status_code: u16,
    body: &str,
    symbols: &[String],
) -> Result<Vec<(String, i128)>, BinancePriceError> {
    if status_code != 200 {
        return Err(BinancePriceError::HttpError(status_code));
    }
    parse_ticker_response_body(body, symbols)
}

pub fn parse_ticker_http_result(
    response: Result<(u16, String), String>,
    symbols: &[String],
) -> Result<Vec<(String, i128)>, BinancePriceError> {
    let (status_code, body) = response.map_err(BinancePriceError::NetworkError)?;
    parse_ticker_http_response(status_code, &body, symbols)
}

pub async fn fetch_spot_prices(
    symbols: &[String],
) -> Result<Vec<(String, i128)>, BinancePriceError> {
    let url = if symbols.len() == 1 {
        format!("{}?symbol={}", BINANCE_TICKER_PRICE_URL, symbols[0])
    } else {
        BINANCE_TICKER_PRICE_URL.to_string()
    };
    let response = crate::http::client()
        .get(&url)
        .send()
        .await
        .map_err(|err| BinancePriceError::NetworkError(err.to_string()))?;
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|err| BinancePriceError::NetworkError(err.to_string()))?;
    parse_ticker_http_result(Ok((status, body)), symbols)
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
    use super::{
        parse_price_to_precision, parse_ticker_http_response, parse_ticker_http_result,
        parse_ticker_response_body, BinancePriceError, FLOAT_PRECISION,
    };

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

    // #345 — rejects negatives and multiple dots
    #[test]
    fn parse_price_rejects_negative() {
        let err = parse_price_to_precision("-1.5").unwrap_err();
        assert!(matches!(err, BinancePriceError::PriceParseError(_)));
    }

    #[test]
    fn parse_price_rejects_multiple_dots() {
        let err = parse_price_to_precision("1.2.3").unwrap_err();
        assert!(matches!(err, BinancePriceError::PriceParseError(_)));
    }

    #[test]
    fn parse_price_correct_scaling_to_1e30() {
        assert_eq!(parse_price_to_precision("1").unwrap(), FLOAT_PRECISION);
        assert_eq!(
            parse_price_to_precision("0.5").unwrap(),
            FLOAT_PRECISION / 2
        );
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

    #[test]
    fn parse_single_ticker_response() {
        let body = r#"{"symbol":"BTCUSDT","price":"60733.99000000"}"#;
        let symbols = vec!["BTCUSDT".to_string()];
        let parsed = parse_ticker_response_body(body, &symbols).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "BTCUSDT".to_string());
        assert_eq!(
            parsed[0].1,
            60733 * FLOAT_PRECISION + 99 * (FLOAT_PRECISION / 100)
        );
    }

    #[test]
    fn parse_ticker_http_response_non_200_returns_error() {
        let symbols = vec!["BTCUSDT".to_string()];
        let err = parse_ticker_http_response(503, "[]", &symbols).unwrap_err();
        assert_eq!(err, BinancePriceError::HttpError(503));
    }

    #[test]
    fn parse_ticker_http_result_network_failure_returns_error() {
        let symbols = vec!["BTCUSDT".to_string()];
        let err = parse_ticker_http_result(Err("timeout".to_string()), &symbols).unwrap_err();
        assert_eq!(err, BinancePriceError::NetworkError("timeout".to_string()));
    }
}
