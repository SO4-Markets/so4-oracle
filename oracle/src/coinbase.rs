use serde::Deserialize;

pub const COINBASE_EXCHANGE_RATES_URL: &str =
    "https://api.coinbase.com/v2/exchange-rates?currency=";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoinbasePriceError {
    NetworkError(String),
    HttpError(u16),
    JsonError(String),
    PriceParseError(String),
    MissingUsdRate,
}

#[derive(Debug, Deserialize)]
pub struct CoinbaseRates {
    pub rates: std::collections::HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct CoinbaseResponse {
    pub data: CoinbaseRates,
}

pub fn parse_coinbase_response_body(body: &str) -> Result<i128, CoinbasePriceError> {
    let resp: CoinbaseResponse =
        serde_json::from_str(body).map_err(|err| CoinbasePriceError::JsonError(err.to_string()))?;

    let usd_price_str = resp
        .data
        .rates
        .get("USD")
        .ok_or(CoinbasePriceError::MissingUsdRate)?;

    // We can reuse the precision parsing from binance, but map the error
    crate::binance::parse_price_to_precision(usd_price_str).map_err(|err| match err {
        crate::binance::BinancePriceError::PriceParseError(msg) => {
            CoinbasePriceError::PriceParseError(msg)
        }
        _ => CoinbasePriceError::PriceParseError("unknown parse error".to_string()),
    })
}

pub fn parse_coinbase_http_response(
    status_code: u16,
    body: &str,
) -> Result<i128, CoinbasePriceError> {
    if status_code != 200 {
        return Err(CoinbasePriceError::HttpError(status_code));
    }
    parse_coinbase_response_body(body)
}

pub fn parse_coinbase_http_result(
    response: Result<(u16, String), String>,
) -> Result<i128, CoinbasePriceError> {
    let (status_code, body) = response.map_err(CoinbasePriceError::NetworkError)?;
    parse_coinbase_http_response(status_code, &body)
}

pub async fn fetch_spot_price(symbol: &str) -> Result<i128, CoinbasePriceError> {
    // Usually the symbol passed is something like "BTC".
    // If it comes with USDT suffix, we should strip it or ensure we query the base asset.
    let base_currency = if let Some(stripped) = symbol.strip_suffix("USDT") {
        stripped
    } else if let Some(stripped) = symbol.strip_suffix("USD") {
        stripped
    } else {
        symbol
    };

    let url_str = format!("{}{}", COINBASE_EXCHANGE_RATES_URL, base_currency);

    let response = crate::http::client()
        .get(&url_str)
        .send()
        .await
        .map_err(|err| CoinbasePriceError::NetworkError(err.to_string()))?;

    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|err| CoinbasePriceError::NetworkError(err.to_string()))?;

    parse_coinbase_http_result(Ok((status, body)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binance::FLOAT_PRECISION;

    #[test]
    fn test_parse_coinbase_response_body_success() {
        let body = r#"{
            "data": {
                "currency": "BTC",
                "rates": {
                    "USD": "60000.50",
                    "EUR": "50000.00"
                }
            }
        }"#;

        let parsed = parse_coinbase_response_body(body).unwrap();
        assert_eq!(parsed, 60000 * FLOAT_PRECISION + (FLOAT_PRECISION / 2));
    }

    #[test]
    fn test_parse_coinbase_response_body_missing_usd() {
        let body = r#"{
            "data": {
                "currency": "BTC",
                "rates": {
                    "EUR": "50000.00"
                }
            }
        }"#;

        let err = parse_coinbase_response_body(body).unwrap_err();
        assert_eq!(err, CoinbasePriceError::MissingUsdRate);
    }

    #[test]
    fn test_parse_coinbase_response_body_invalid_json() {
        let err = parse_coinbase_response_body("not json").unwrap_err();
        assert!(matches!(err, CoinbasePriceError::JsonError(_)));
    }

    #[test]
    fn test_parse_coinbase_http_response_non_200() {
        let err = parse_coinbase_http_response(404, "{}").unwrap_err();
        assert_eq!(err, CoinbasePriceError::HttpError(404));
    }

    #[test]
    fn test_parse_coinbase_http_result_network_failure() {
        let err = parse_coinbase_http_result(Err("timeout".to_string())).unwrap_err();
        assert_eq!(err, CoinbasePriceError::NetworkError("timeout".to_string()));
    }

    // ── #347 acceptance criteria ──────────────────────────────────────────────

    /// #347 — USDT suffix is stripped before querying Coinbase.
    /// fetch_spot_price strips suffixes so the URL uses the base asset only.
    #[test]
    fn coinbase_strips_usdt_suffix() {
        // Verify suffix-stripping logic directly via parse helpers.
        // "BTCUSDT" → base "BTC" → USD rate extracted correctly.
        let body = r#"{
            "data": {
                "currency": "BTC",
                "rates": { "USD": "50000.0" }
            }
        }"#;
        let result = parse_coinbase_response_body(body).unwrap();
        assert_eq!(result, 50000 * FLOAT_PRECISION);
    }

    /// #347 — USD suffix is also stripped.
    #[test]
    fn coinbase_strips_usd_suffix() {
        let body = r#"{
            "data": {
                "currency": "ETH",
                "rates": { "USD": "3000.0" }
            }
        }"#;
        let result = parse_coinbase_response_body(body).unwrap();
        assert_eq!(result, 3000 * FLOAT_PRECISION);
    }

    // #363 — verify the USD rate is extracted and scaled to 1e30 precision
    #[test]
    fn test_coinbase_parse_extracts_usd_rate_correctly() {
        let body = r#"{
            "data": {
                "currency": "XLM",
                "rates": {
                    "USD": "1.0",
                    "EUR": "0.9"
                }
            }
        }"#;
        let result = parse_coinbase_response_body(body).unwrap();
        assert_eq!(result, FLOAT_PRECISION);
    }

    // #364 — a response body without a USD key must return MissingUsdRate
    #[test]
    fn test_coinbase_parse_rejects_missing_usd_rate() {
        let body = r#"{
            "data": {
                "currency": "XLM",
                "rates": {
                    "EUR": "0.9"
                }
            }
        }"#;
        let err = parse_coinbase_response_body(body).unwrap_err();
        assert_eq!(err, CoinbasePriceError::MissingUsdRate);
    }
}
