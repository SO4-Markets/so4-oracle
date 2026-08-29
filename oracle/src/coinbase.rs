use serde::Deserialize;

pub const COINBASE_EXCHANGE_RATES_URL: &str =
    "https://api.coinbase.com/v2/exchange-rates?currency=";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoinbasePriceError {
    NetworkError(String),
    HttpError { status: u16, body: String },
    JsonError(String),
    PriceParseError(String),
    MissingUsdRate,
    CurrencyMismatch { expected: String, got: String },
}

impl std::fmt::Display for CoinbasePriceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NetworkError(error) => write!(f, "Coinbase network error: {error}"),
            Self::HttpError { status, body } => {
                if body.is_empty() {
                    write!(f, "Coinbase returned HTTP {status}")
                } else {
                    write!(f, "Coinbase returned HTTP {status}: {body}")
                }
            }
            Self::JsonError(error) => write!(f, "invalid Coinbase response: {error}"),
            Self::PriceParseError(error) => write!(f, "invalid Coinbase price: {error}"),
            Self::MissingUsdRate => f.write_str("Coinbase response has no USD rate"),
            Self::CurrencyMismatch { expected, got } => {
                write!(
                    f,
                    "Coinbase currency mismatch: expected '{}', got '{}'",
                    expected, got
                )
            }
        }
    }
}

impl std::error::Error for CoinbasePriceError {}

impl crate::retry::Retryable for CoinbasePriceError {
    fn is_retryable(&self) -> bool {
        match self {
            // Network errors and 5xx HTTP errors are transient
            Self::NetworkError(_) => true,
            Self::HttpError { status, .. } => *status >= 500,
            // Parse/JSON/config errors are permanent failures
            Self::JsonError(_) | Self::PriceParseError(_) | Self::MissingUsdRate => false,
            // Currency mismatch should not be retried - it's a data integrity issue
            Self::CurrencyMismatch { .. } => false,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CoinbaseResponse {
    pub data: CoinbaseResponseData,
}

#[derive(Debug, Deserialize)]
pub struct CoinbaseResponseData {
    pub currency: String,
    pub rates: std::collections::HashMap<String, String>,
}

pub fn parse_coinbase_response_body(
    body: &str,
    expected_currency: &str,
) -> Result<i128, CoinbasePriceError> {
    let resp: CoinbaseResponse =
        serde_json::from_str(body).map_err(|err| CoinbasePriceError::JsonError(err.to_string()))?;

    // Validate that the returned currency matches what we requested
    if resp.data.currency.to_uppercase() != expected_currency.to_uppercase() {
        return Err(CoinbasePriceError::CurrencyMismatch {
            expected: expected_currency.to_string(),
            got: resp.data.currency,
        });
    }

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
    expected_currency: &str,
) -> Result<i128, CoinbasePriceError> {
    if status_code != 200 {
        return Err(CoinbasePriceError::HttpError {
            status: status_code,
            body: crate::http::truncate_error_body(body),
        });
    }
    parse_coinbase_response_body(body, expected_currency)
}

pub fn parse_coinbase_http_result(
    response: Result<(u16, String), String>,
    expected_currency: &str,
) -> Result<i128, CoinbasePriceError> {
    let (status_code, body) = response.map_err(CoinbasePriceError::NetworkError)?;
    parse_coinbase_http_response(status_code, &body, expected_currency)
}

pub async fn fetch_spot_price(symbol: &str) -> Result<i128, CoinbasePriceError> {
    fetch_spot_price_with_url(COINBASE_EXCHANGE_RATES_URL, symbol).await
}

pub(crate) async fn fetch_spot_price_with_url(
    base_url: &str,
    symbol: &str,
) -> Result<i128, CoinbasePriceError> {
    // Usually the symbol passed is something like "BTC".
    // If it comes with USDT/USD suffix, strip it to get the base asset.
    // Guard against stripping leaving an empty string (e.g. symbol "USDT"
    // or "USD" exactly) — fall back to the original symbol in that case.
    let base_currency = symbol
        .strip_suffix("USDT")
        .or_else(|| symbol.strip_suffix("USD"))
        .filter(|stripped| !stripped.is_empty())
        .unwrap_or(symbol);

    let (clean_url, use_query) =
        if base_url.contains("currency=") || base_url.contains("exchange-rates") {
            (
                base_url
                    .trim_end_matches("?currency=")
                    .trim_end_matches("&currency="),
                true,
            )
        } else {
            (base_url, false)
        };

    let response = if use_query {
        crate::http::client()
            .get(clean_url)
            .query(&[("currency", base_currency)])
            .send()
            .await
    } else {
        let url_str = format!("{}{}", clean_url, base_currency);
        crate::http::client().get(&url_str).send().await
    }
    .map_err(|err| CoinbasePriceError::NetworkError(err.to_string()))?;

    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|err| CoinbasePriceError::NetworkError(err.to_string()))?;

    parse_coinbase_http_result(Ok((status, body)), base_currency)
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

        let parsed = parse_coinbase_response_body(body, "BTC").unwrap();
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

        let err = parse_coinbase_response_body(body, "BTC").unwrap_err();
        assert_eq!(err, CoinbasePriceError::MissingUsdRate);
    }

    #[test]
    fn test_parse_coinbase_response_body_currency_mismatch() {
        let body = r#"{
            "data": {
                "currency": "ETH",
                "rates": {
                    "USD": "50000.00"
                }
            }
        }"#;

        let err = parse_coinbase_response_body(body, "BTC").unwrap_err();
        assert!(matches!(
            err,
            CoinbasePriceError::CurrencyMismatch { expected, got }
            if expected == "BTC" && got == "ETH"
        ));
    }

    // #604 — Coinbase reuses binance::parse_price_to_precision, so a "USD" rate
    // of exactly zero must surface as a parse error, not a valid price of 0.
    #[test]
    fn test_parse_coinbase_response_body_rejects_zero_usd_rate() {
        let body = r#"{
            "data": {
                "currency": "BTC",
                "rates": {
                    "USD": "0"
                }
            }
        }"#;

        let err = parse_coinbase_response_body(body, "BTC").unwrap_err();
        assert!(matches!(err, CoinbasePriceError::PriceParseError(_)));
    }

    #[test]
    fn test_parse_coinbase_response_body_invalid_json() {
        let err = parse_coinbase_response_body("not json", "BTC").unwrap_err();
        assert!(matches!(err, CoinbasePriceError::JsonError(_)));
    }

    #[test]
    fn test_parse_coinbase_http_response_non_200() {
        let err = parse_coinbase_http_response(404, "{}", "BTC").unwrap_err();
        assert!(matches!(
            err,
            CoinbasePriceError::HttpError { status: 404, .. }
        ));
    }

    #[test]
    fn test_parse_coinbase_http_result_network_failure() {
        let err = parse_coinbase_http_result(Err("timeout".to_string()), "BTC").unwrap_err();
        assert_eq!(err, CoinbasePriceError::NetworkError("timeout".to_string()));
    }

    // ── #347 acceptance criteria ──────────────────────────────────────────────

    /// #347 / #794 — a raw "SOLUSDT" symbol must be stripped to "SOL" *before*
    /// the Coinbase request is built. Drives the real `fetch_spot_price_with_url`
    /// via wiremock and asserts the outgoing request path is the stripped base,
    /// instead of re-checking `parse_coinbase_response_body` with an
    /// already-stripped symbol (which never exercises the strip at all).
    #[tokio::test]
    async fn coinbase_strips_usdt_suffix() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = r#"{
            "data": {
                "currency": "SOL",
                "rates": { "USD": "150.0" }
            }
        }"#;
        Mock::given(method("GET"))
            .and(path("/SOL"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let base_url = format!("{}/", server.uri());
        let result = super::fetch_spot_price_with_url(&base_url, "SOLUSDT")
            .await
            .unwrap();
        assert_eq!(result, 150 * FLOAT_PRECISION);

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url.path(),
            "/SOL",
            "the USDT suffix must be stripped from the request URL"
        );
    }

    /// #347 / #794 — the "USD" suffix is stripped the same way.
    #[tokio::test]
    async fn coinbase_strips_usd_suffix() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = r#"{
            "data": {
                "currency": "ADA",
                "rates": { "USD": "0.5" }
            }
        }"#;
        Mock::given(method("GET"))
            .and(path("/ADA"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let base_url = format!("{}/", server.uri());
        let result = super::fetch_spot_price_with_url(&base_url, "ADAUSD")
            .await
            .unwrap();
        assert_eq!(result, FLOAT_PRECISION / 2);

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url.path(),
            "/ADA",
            "the USD suffix must be stripped from the request URL"
        );
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
        let result = parse_coinbase_response_body(body, "XLM").unwrap();
        assert_eq!(result, FLOAT_PRECISION);
    }

    // ── exact-match USDT/USD regression tests ────────────────────────────────

    /// When the configured coinbase_symbol is exactly "USDT", strip_suffix("USDT")
    /// must NOT produce an empty base currency — the symbol itself should be used.
    #[test]
    fn coinbase_exact_usdt_symbol_uses_symbol_not_empty() {
        let body = r#"{
            "data": {
                "currency": "USDT",
                "rates": { "USD": "1.0" }
            }
        }"#;
        let result = parse_coinbase_response_body(body, "USDT").unwrap();
        assert_eq!(result, FLOAT_PRECISION);
    }

    /// When the configured coinbase_symbol is exactly "USD", strip_suffix("USD")
    /// must NOT produce an empty base currency — the symbol itself should be used.
    #[test]
    fn coinbase_exact_usd_symbol_uses_symbol_not_empty() {
        let body = r#"{
            "data": {
                "currency": "USD",
                "rates": { "USD": "1.0" }
            }
        }"#;
        let result = parse_coinbase_response_body(body, "USD").unwrap();
        assert_eq!(result, FLOAT_PRECISION);
    }

    /// #795 — when the symbol is exactly "USDT", stripping "USDT" would leave an
    /// empty base. `fetch_spot_price_with_url` must fall back to the symbol
    /// itself and query Coinbase for "USDT", never "". Exercises the real
    /// function via wiremock instead of copy-pasting the strip expression into
    /// the test body (which could never catch a regression in the real code).
    #[tokio::test]
    async fn coinbase_exact_usdt_strips_to_self() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = r#"{
            "data": {
                "currency": "USDT",
                "rates": { "USD": "1.0" }
            }
        }"#;
        Mock::given(method("GET"))
            .and(path("/USDT"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let base_url = format!("{}/", server.uri());
        let result = super::fetch_spot_price_with_url(&base_url, "USDT")
            .await
            .unwrap();
        assert_eq!(result, FLOAT_PRECISION);

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url.path(),
            "/USDT",
            "an exact \"USDT\" symbol must not be stripped to an empty request path"
        );
    }

    /// #795 — same degenerate case for an exact "USD" symbol.
    #[tokio::test]
    async fn coinbase_exact_usd_strips_to_self() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = r#"{
            "data": {
                "currency": "USD",
                "rates": { "USD": "1.0" }
            }
        }"#;
        Mock::given(method("GET"))
            .and(path("/USD"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let base_url = format!("{}/", server.uri());
        let result = super::fetch_spot_price_with_url(&base_url, "USD")
            .await
            .unwrap();
        assert_eq!(result, FLOAT_PRECISION);

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url.path(),
            "/USD",
            "an exact \"USD\" symbol must not be stripped to an empty request path"
        );
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
        let err = parse_coinbase_response_body(body, "XLM").unwrap_err();
        assert_eq!(err, CoinbasePriceError::MissingUsdRate);
    }

    /// Test that case-insensitive currency validation works
    #[test]
    fn test_coinbase_currency_case_insensitive() {
        let body = r#"{
            "data": {
                "currency": "btc",
                "rates": { "USD": "50000.0" }
            }
        }"#;
        let result = parse_coinbase_response_body(body, "BTC").unwrap();
        assert_eq!(result, 50000 * FLOAT_PRECISION);
    }

    /// Test that parse_coinbase_http_response validates currency
    #[test]
    fn test_parse_coinbase_http_response_currency_validation() {
        let body = r#"{
            "data": {
                "currency": "BTC",
                "rates": { "USD": "50000.0" }
            }
        }"#;
        let result = parse_coinbase_http_response(200, body, "BTC").unwrap();
        assert_eq!(result, 50000 * FLOAT_PRECISION);
    }

    #[test]
    fn test_parse_coinbase_http_response_wrong_currency() {
        let body = r#"{
            "data": {
                "currency": "ETH",
                "rates": { "USD": "50000.0" }
            }
        }"#;
        let err = parse_coinbase_http_response(200, body, "BTC").unwrap_err();
        assert!(matches!(
            err,
            CoinbasePriceError::CurrencyMismatch { expected, got }
            if expected == "BTC" && got == "ETH"
        ));
    }

    // ── HTTP-level wiremock tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn fetch_spot_price_strips_usdt_suffix_success() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = r#"{
            "data": {
                "currency": "BTC",
                "rates": { "USD": "50000.0" }
            }
        }"#;

        // "BTCUSDT" → base "BTC" → URL path should be "/BTC"
        Mock::given(method("GET"))
            .and(path("/BTC"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let base_url = format!("{}/", server.uri());
        let result = super::fetch_spot_price_with_url(&base_url, "BTCUSDT")
            .await
            .unwrap();
        assert_eq!(result, 50000 * FLOAT_PRECISION);
    }

    #[tokio::test]
    async fn fetch_spot_price_strips_usd_suffix_success() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = r#"{
            "data": {
                "currency": "ETH",
                "rates": { "USD": "3000.0" }
            }
        }"#;

        Mock::given(method("GET"))
            .and(path("/ETH"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let base_url = format!("{}/", server.uri());
        let result = super::fetch_spot_price_with_url(&base_url, "ETHUSD")
            .await
            .unwrap();
        assert_eq!(result, 3000 * FLOAT_PRECISION);
    }

    #[tokio::test]
    async fn fetch_spot_price_no_suffix_success() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = r#"{
            "data": {
                "currency": "XLM",
                "rates": { "USD": "1.0" }
            }
        }"#;

        Mock::given(method("GET"))
            .and(path("/XLM"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let base_url = format!("{}/", server.uri());
        let result = super::fetch_spot_price_with_url(&base_url, "XLM")
            .await
            .unwrap();
        assert_eq!(result, FLOAT_PRECISION);
    }

    #[tokio::test]
    async fn fetch_spot_price_404_returns_http_error() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let base_url = format!("{}/", server.uri());
        let err = super::fetch_spot_price_with_url(&base_url, "BTC")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            CoinbasePriceError::HttpError { status: 404, .. }
        ));
    }

    #[tokio::test]
    async fn fetch_spot_price_500_returns_http_error() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let base_url = format!("{}/", server.uri());
        let err = super::fetch_spot_price_with_url(&base_url, "BTC")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            CoinbasePriceError::HttpError { status: 500, .. }
        ));
    }

    /// #706 — every other HTTP test builds `base_url` as `"{uri}/"`, which
    /// contains neither `"currency="` nor `"exchange-rates"`, so they all
    /// exercise the `use_query = false` (`format!`-concatenated) branch that
    /// production never takes. The real `COINBASE_EXCHANGE_RATES_URL` shape
    /// (`.../v2/exchange-rates?currency=`) drives `use_query = true` — the
    /// `reqwest .query()` path. This test covers that branch at the HTTP
    /// level: `?currency=` is trimmed, and the currency arrives as a real
    /// query parameter.
    #[tokio::test]
    async fn fetch_spot_price_production_url_shape_uses_query_param_branch() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = r#"{
            "data": {
                "currency": "BTC",
                "rates": { "USD": "50000.0" }
            }
        }"#;

        Mock::given(method("GET"))
            .and(path("/v2/exchange-rates"))
            .and(query_param("currency", "BTC"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        // Same shape as the real COINBASE_EXCHANGE_RATES_URL constant.
        let base_url = format!("{}/v2/exchange-rates?currency=", server.uri());
        let result = super::fetch_spot_price_with_url(&base_url, "BTCUSDT")
            .await
            .unwrap();
        assert_eq!(result, 50000 * FLOAT_PRECISION);

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url.path(), "/v2/exchange-rates");
        assert_eq!(
            requests[0].url.query(),
            Some("currency=BTC"),
            "currency must be sent as a query parameter, not concatenated into the path"
        );
    }
}
