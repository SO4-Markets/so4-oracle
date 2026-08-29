use serde::Deserialize;
use std::collections::HashMap;

/// Provider endpoint recommended by Pyth for authenticated production traffic.
pub const PYTH_HERMES_URL: &str = "https://pyth.dourolabs.app/hermes/api/latest_price_feeds";

/// Re-exported from the crate root so existing `crate::pyth::FLOAT_PRECISION`
/// callers keep working; the precision invariant has one source of truth now
/// (#709) rather than a separate copy per module.
pub use crate::FLOAT_PRECISION;

#[derive(Debug, Clone, PartialEq)]
pub enum PythPriceError {
    NetworkError(String),
    HttpError {
        status: u16,
        body: String,
    },
    JsonError(String),
    PriceParseError(String),
    MissingFeedId(String),
    StalePrice {
        age_seconds: u64,
        max_age_seconds: u64,
    },
    ConfidenceTooWide {
        confidence_bps: f64,
        max_bps: u32,
    },
    MissingPublishTime,
    InvalidPublishTime(i64),
}

impl std::fmt::Display for PythPriceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NetworkError(error) => write!(f, "Pyth network error: {error}"),
            Self::HttpError { status, body } => {
                if body.is_empty() {
                    write!(f, "Pyth returned HTTP {status}")
                } else {
                    write!(f, "Pyth returned HTTP {status}: {body}")
                }
            }
            Self::JsonError(error) => write!(f, "invalid Pyth response: {error}"),
            Self::PriceParseError(error) => write!(f, "invalid Pyth price: {error}"),
            Self::MissingFeedId(id) => write!(f, "Pyth response is missing feed {id}"),
            Self::StalePrice {
                age_seconds,
                max_age_seconds,
            } => write!(
                f,
                "Pyth price is stale ({age_seconds}s; maximum {max_age_seconds}s)"
            ),
            Self::ConfidenceTooWide {
                confidence_bps,
                max_bps,
            } => write!(
                f,
                "Pyth confidence interval is too wide ({confidence_bps:.2} bps; maximum {max_bps})"
            ),
            Self::MissingPublishTime => {
                write!(f, "Pyth response is missing publish_time field")
            }
            Self::InvalidPublishTime(value) => write!(f, "invalid Pyth publish time: {value}"),
        }
    }
}

impl std::error::Error for PythPriceError {}

impl crate::retry::Retryable for PythPriceError {
    fn is_retryable(&self) -> bool {
        match self {
            // Network errors and 5xx HTTP errors are transient
            Self::NetworkError(_) => true,
            Self::HttpError { status, .. } => *status >= 500,
            // Stale prices might become fresh on retry
            Self::StalePrice { .. } => true,
            // Parse/JSON/config/validation errors are permanent failures
            Self::JsonError(_)
            | Self::PriceParseError(_)
            | Self::MissingFeedId(_)
            | Self::ConfidenceTooWide { .. }
            | Self::MissingPublishTime
            | Self::InvalidPublishTime(_) => false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PythPriceData {
    pub price: String,
    #[serde(default)]
    pub conf: Option<String>,
    pub expo: i32,
    #[serde(default)]
    pub publish_time: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PythPriceFeed {
    pub id: String,
    pub price: PythPriceData,
}

#[derive(Debug, Deserialize)]
pub struct PythResponse {
    pub data: PythPriceFeed,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum HermesResponse {
    Array(Vec<PythPriceFeed>),
    Wrapped(PythResponse),
}

pub fn normalize_pyth_price(price_str: &str, exponent: i32) -> Result<i128, PythPriceError> {
    // Pyth exponents are negative (or zero); anything below -SCALE_DIGITS
    // can't be represented at our on-chain precision (#709).
    let min_exponent = -(crate::SCALE_DIGITS as i32);
    if !(min_exponent..=0).contains(&exponent) {
        return Err(PythPriceError::PriceParseError(format!(
            "unsupported exponent: {exponent}"
        )));
    }

    let price_int = price_str
        .trim()
        .parse::<i128>()
        .map_err(|_| PythPriceError::PriceParseError(format!("invalid price: {}", price_str)))?;

    if price_int < 0 {
        return Err(PythPriceError::PriceParseError(
            "negative prices not supported".to_string(),
        ));
    }

    let exponent_diff = crate::SCALE_DIGITS as i32 + exponent;

    if exponent_diff >= 0 {
        price_int
            .checked_mul(10i128.pow(exponent_diff as u32))
            .ok_or_else(|| {
                PythPriceError::PriceParseError("price overflow during normalization".to_string())
            })
    } else {
        let divisor = 10i128.pow((-exponent_diff) as u32);
        Ok(price_int / divisor)
    }
}

pub fn validate_pyth_price(
    data: &PythPriceData,
    now_seconds: u64,
    stale_after_seconds: u64,
    max_confidence_bps: u32,
) -> Result<i128, PythPriceError> {
    let price = normalize_pyth_price(&data.price, data.expo)?;
    let publish_time = data
        .publish_time
        .ok_or(PythPriceError::MissingPublishTime)?;
    if publish_time < 0 {
        return Err(PythPriceError::InvalidPublishTime(publish_time));
    }
    let publish_time = publish_time as u64;
    let age_seconds = now_seconds.saturating_sub(publish_time);
    if age_seconds > stale_after_seconds {
        return Err(PythPriceError::StalePrice {
            age_seconds,
            max_age_seconds: stale_after_seconds,
        });
    }

    // #603 — the zero-price guard must hold regardless of whether the feed
    // carried a `conf` field. Hermes responses that omit `conf` skip the
    // confidence check by design; they must not skip this invariant with it.
    if price <= 0 {
        return Err(PythPriceError::PriceParseError(
            "price must be greater than zero".to_string(),
        ));
    }

    if let Some(conf) = &data.conf {
        let confidence = normalize_pyth_price(conf, data.expo)?;
        let confidence_bps = (confidence as f64 / price as f64) * 10_000.0;
        if confidence_bps > max_confidence_bps as f64 {
            return Err(PythPriceError::ConfidenceTooWide {
                confidence_bps,
                max_bps: max_confidence_bps,
            });
        }
    }

    Ok(price)
}

// URL-injecting twin of the batched fetch path, used by the mock-server tests below.
#[cfg(test)]
pub(crate) async fn fetch_pyth_price_with_url(
    base_url: &str,
    feed_id: &str,
    stale_after_seconds: u64,
    max_confidence_bps: u32,
) -> Result<i128, PythPriceError> {
    let query = format!("ids[]={feed_id}");
    let url_string = format!("{base_url}?{query}");

    let response = crate::http::client()
        .get(&url_string)
        .send()
        .await
        .map_err(|err| PythPriceError::NetworkError(err.to_string()))?;

    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|err| PythPriceError::NetworkError(err.to_string()))?;

    if status != 200 {
        return Err(PythPriceError::HttpError {
            status,
            body: crate::http::truncate_error_body(&body),
        });
    }

    let hermes_response: HermesResponse =
        serde_json::from_str(&body).map_err(|err| PythPriceError::JsonError(err.to_string()))?;
    let feeds = match hermes_response {
        HermesResponse::Array(feeds) => feeds,
        HermesResponse::Wrapped(wrapped) => vec![wrapped.data],
    };
    let feed = feeds
        .into_iter()
        .find(|f| f.id == feed_id)
        .ok_or_else(|| PythPriceError::MissingFeedId(feed_id.to_string()))?;
    validate_pyth_price(
        &feed.price,
        crate::current_timestamp_secs(),
        stale_after_seconds,
        max_confidence_bps,
    )
}

/// Fetches all requested feeds in a single Hermes request. The bearer token is
/// optional during the migration period, but production deployments should set
/// `PYTH_API_KEY` before authentication becomes mandatory.
pub async fn fetch_pyth_prices(
    feed_ids: &[&str],
    api_key: Option<&str>,
) -> Result<HashMap<String, PythPriceFeed>, PythPriceError> {
    if feed_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let query = feed_ids
        .iter()
        .map(|id| format!("ids[]={id}"))
        .collect::<Vec<_>>()
        .join("&");
    let url_string = format!("{PYTH_HERMES_URL}?{query}");

    let mut request = crate::http::client().get(&url_string);
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|err| PythPriceError::NetworkError(err.to_string()))?;

    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|err| PythPriceError::NetworkError(err.to_string()))?;

    if status != 200 {
        return Err(PythPriceError::HttpError {
            status,
            body: crate::http::truncate_error_body(&body),
        });
    }

    let response: HermesResponse =
        serde_json::from_str(&body).map_err(|err| PythPriceError::JsonError(err.to_string()))?;
    let feeds = match response {
        HermesResponse::Array(feeds) => feeds,
        HermesResponse::Wrapped(wrapped) => vec![wrapped.data],
    };
    Ok(feeds
        .into_iter()
        .map(|feed| (feed.id.clone(), feed))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_pyth_price_positive_exponent() {
        let err = normalize_pyth_price("45000000000", 8).unwrap_err();
        assert!(matches!(err, PythPriceError::PriceParseError(_)));
    }

    #[test]
    fn normalize_pyth_price_negative_exponent() {
        let result = normalize_pyth_price("4500000000", -8).unwrap();
        assert!(result > 0);
    }

    #[test]
    fn normalize_pyth_price_invalid() {
        let err = normalize_pyth_price("invalid", -8).unwrap_err();
        assert!(matches!(err, PythPriceError::PriceParseError(_)));
    }

    #[test]
    fn normalize_pyth_price_negative() {
        let err = normalize_pyth_price("-45000000000", -8).unwrap_err();
        assert!(matches!(err, PythPriceError::PriceParseError(_)));
    }

    #[test]
    fn validate_pyth_price_accepts_fresh_confident_price() {
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: Some("100000".to_string()),
            expo: -8,
            publish_time: Some(1_000),
        };
        let price = validate_pyth_price(&data, 1_010, 60, 50).unwrap();
        assert_eq!(price, FLOAT_PRECISION);
    }

    #[test]
    fn validate_pyth_price_rejects_stale_price() {
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: Some("100000".to_string()),
            expo: -8,
            publish_time: Some(1_000),
        };
        let err = validate_pyth_price(&data, 1_500, 60, 50).unwrap_err();
        assert!(matches!(err, PythPriceError::StalePrice { .. }));
    }

    #[test]
    fn validate_pyth_price_rejects_wide_confidence() {
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: Some("10000000".to_string()),
            expo: -8,
            publish_time: Some(1_000),
        };
        let err = validate_pyth_price(&data, 1_010, 60, 50).unwrap_err();
        assert!(matches!(err, PythPriceError::ConfidenceTooWide { .. }));
    }

    #[test]
    fn validate_pyth_price_rejects_missing_publish_time() {
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: Some("100000".to_string()),
            expo: -8,
            publish_time: None,
        };
        let err = validate_pyth_price(&data, 1_010, 60, 50).unwrap_err();
        assert_eq!(err, PythPriceError::MissingPublishTime);
    }

    #[test]
    fn validate_pyth_price_rejects_negative_publish_time() {
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: Some("100000".to_string()),
            expo: -8,
            publish_time: Some(-1),
        };
        let err = validate_pyth_price(&data, 1_010, 60, 50).unwrap_err();
        assert_eq!(err, PythPriceError::InvalidPublishTime(-1));
    }

    #[test]
    fn validate_pyth_price_rejects_zero_price_when_confidence_present() {
        let data = PythPriceData {
            price: "0".to_string(),
            conf: Some("100000".to_string()),
            expo: -8,
            publish_time: Some(1_000),
        };
        let err = validate_pyth_price(&data, 1_010, 60, 50).unwrap_err();
        assert!(matches!(err, PythPriceError::PriceParseError(_)));
    }

    // #603 — a feed that omits `conf` skips the confidence check; it must not
    // skip the zero-price guard along with it.
    #[test]
    fn validate_pyth_price_rejects_zero_price_when_confidence_absent() {
        let data = PythPriceData {
            price: "0".to_string(),
            conf: None,
            expo: -8,
            publish_time: Some(1_000),
        };
        let err = validate_pyth_price(&data, 1_010, 60, 50).unwrap_err();
        assert!(matches!(err, PythPriceError::PriceParseError(_)));
    }

    #[test]
    fn normalize_pyth_price_rejects_overflow() {
        let err = normalize_pyth_price(&i128::MAX.to_string(), 0).unwrap_err();
        assert!(matches!(err, PythPriceError::PriceParseError(_)));
    }

    // ── Staleness validation (#351) ───────────────────────────────────────────

    #[test]
    fn staleness_accepts_price_published_exactly_at_stale_boundary() {
        // age == stale_after_seconds is NOT stale (only strictly greater is rejected)
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: None,
            expo: -8,
            publish_time: Some(1_000),
        };
        // now=1060, publish=1000 → age=60 == stale_after_seconds=60 → accepted
        let result = validate_pyth_price(&data, 1_060, 60, 100);
        assert!(result.is_ok());
    }

    #[test]
    fn staleness_rejects_price_one_second_past_boundary() {
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: None,
            expo: -8,
            publish_time: Some(1_000),
        };
        // now=1061, publish=1000 → age=61 > stale_after_seconds=60 → rejected
        let err = validate_pyth_price(&data, 1_061, 60, 100).unwrap_err();
        assert!(matches!(
            err,
            PythPriceError::StalePrice {
                age_seconds: 61,
                max_age_seconds: 60,
            }
        ));
    }

    #[test]
    fn staleness_accepts_price_published_one_second_ago() {
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: None,
            expo: -8,
            publish_time: Some(999),
        };
        // now=1000, publish=999 → age=1 < stale_after_seconds=60 → accepted
        let result = validate_pyth_price(&data, 1_000, 60, 100);
        assert!(result.is_ok());
    }

    // ── Confidence validation (#352) ──────────────────────────────────────────

    #[test]
    fn confidence_accepts_price_with_no_confidence_field() {
        // Missing confidence (None) bypasses the bps check entirely
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: None,
            expo: -8,
            publish_time: Some(1_000),
        };
        let result = validate_pyth_price(&data, 1_010, 60, 0);
        assert!(result.is_ok());
    }

    #[test]
    fn confidence_accepts_price_at_exact_threshold() {
        // confidence == max_confidence_bps is within threshold (only strictly greater rejected)
        // price=100000000, conf=50000 → confidence_bps = (50000/100000000)*10000 = 5.0 bps
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: Some("50000".to_string()),
            expo: -8,
            publish_time: Some(1_000),
        };
        // max_bps=5 → 5.0 <= 5 → accepted (5.0 > 5 is false)
        let result = validate_pyth_price(&data, 1_010, 60, 5);
        assert!(result.is_ok());
    }

    #[test]
    fn confidence_rejects_confidence_one_bps_over_threshold() {
        // price=100000000, conf=60000 → confidence_bps = 6.0 bps > max_bps=5 → rejected
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: Some("60000".to_string()),
            expo: -8,
            publish_time: Some(1_000),
        };
        let err = validate_pyth_price(&data, 1_010, 60, 5).unwrap_err();
        assert!(matches!(err, PythPriceError::ConfidenceTooWide { .. }));
    }

    // ── Publish time rejection (#353) ─────────────────────────────────────────

    #[test]
    fn publish_time_accepts_zero_as_valid_non_negative_timestamp() {
        // publish_time=0 is valid (>= 0), even though it's the Unix epoch
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: None,
            expo: -8,
            publish_time: Some(0),
        };
        // now=60 → age=60 == stale_after=60 → accepted
        let result = validate_pyth_price(&data, 60, 60, 100);
        assert!(result.is_ok());
    }

    #[test]
    fn publish_time_rejects_large_negative_value() {
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: None,
            expo: -8,
            publish_time: Some(i64::MIN),
        };
        let err = validate_pyth_price(&data, 1_010, 60, 100).unwrap_err();
        assert!(matches!(err, PythPriceError::InvalidPublishTime(t) if t == i64::MIN));
    }

    #[test]
    fn publish_time_rejects_minus_one() {
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: None,
            expo: -8,
            publish_time: Some(-1),
        };
        let err = validate_pyth_price(&data, 1_010, 60, 100).unwrap_err();
        assert_eq!(err, PythPriceError::InvalidPublishTime(-1));
    }

    // #349 — the Hermes response parser handles both array and wrapped formats

    #[test]
    fn hermes_response_deserializes_array_format() {
        let json = r#"[{"id":"abc123","price":{"price":"4500000000","expo":-8,"conf":"100000","publish_time":1000}}]"#;
        let response: HermesResponse = serde_json::from_str(json).unwrap();
        let feed = match response {
            HermesResponse::Array(mut feeds) => feeds.pop().unwrap(),
            HermesResponse::Wrapped(_) => panic!("expected array format"),
        };
        assert_eq!(feed.id, "abc123");
        assert_eq!(feed.price.expo, -8);
    }

    #[test]
    fn hermes_response_deserializes_wrapped_format() {
        let json = r#"{"data":{"id":"abc123","price":{"price":"4500000000","expo":-8,"conf":"100000","publish_time":1000}}}"#;
        let response: HermesResponse = serde_json::from_str(json).unwrap();
        let feed = match response {
            HermesResponse::Wrapped(w) => w.data,
            HermesResponse::Array(_) => panic!("expected wrapped format"),
        };
        assert_eq!(feed.id, "abc123");
        assert_eq!(feed.price.expo, -8);
    }

    #[test]
    fn hermes_response_array_empty_is_parseable() {
        let json = r#"[]"#;
        let response: HermesResponse = serde_json::from_str(json).unwrap();
        assert!(matches!(response, HermesResponse::Array(v) if v.is_empty()));
    }

    // ── HTTP-level wiremock tests ─────────────────────────────────────────────

    fn recent_publish_time() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[tokio::test]
    async fn fetch_pyth_price_array_format_success() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let publish = recent_publish_time();
        let body = format!(
            r#"[{{"id":"feed-1","price":{{"price":"4500000000","expo":-8,"conf":"100000","publish_time":{}}}}}]"#,
            publish
        );

        Mock::given(method("GET"))
            .and(query_param("ids[]", "feed-1"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let result = super::fetch_pyth_price_with_url(&server.uri(), "feed-1", 60, 50)
            .await
            .unwrap();
        assert_eq!(result, 45 * FLOAT_PRECISION);
    }

    #[tokio::test]
    async fn fetch_pyth_price_wrapped_format_success() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let publish = recent_publish_time();
        let body = format!(
            r#"{{"data":{{"id":"feed-1","price":{{"price":"4500000000","expo":-8,"conf":"100000","publish_time":{}}}}}}}"#,
            publish
        );

        Mock::given(method("GET"))
            .and(query_param("ids[]", "feed-1"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let result = super::fetch_pyth_price_with_url(&server.uri(), "feed-1", 60, 50)
            .await
            .unwrap();
        assert_eq!(result, 45 * FLOAT_PRECISION);
    }

    // #603 — the exact trigger from the report: a fresh feed whose body omits
    // `conf` entirely and carries a price of zero must be rejected, not fetched
    // as a valid price.
    #[tokio::test]
    async fn fetch_pyth_price_rejects_zero_price_with_missing_conf_field() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let publish = recent_publish_time();
        let body = format!(
            r#"[{{"id":"feed-1","price":{{"price":"0","expo":-8,"publish_time":{}}}}}]"#,
            publish
        );

        Mock::given(method("GET"))
            .and(query_param("ids[]", "feed-1"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let err = super::fetch_pyth_price_with_url(&server.uri(), "feed-1", 60, 50)
            .await
            .unwrap_err();
        assert!(matches!(err, PythPriceError::PriceParseError(_)));
    }

    #[tokio::test]
    async fn fetch_pyth_price_empty_array_returns_missing_feed_id() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = r#"[]"#;

        Mock::given(method("GET"))
            .and(query_param("ids[]", "feed-1"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let err = super::fetch_pyth_price_with_url(&server.uri(), "feed-1", 60, 50)
            .await
            .unwrap_err();
        assert!(matches!(err, PythPriceError::MissingFeedId(id) if id == "feed-1"));
    }

    #[tokio::test]
    async fn fetch_pyth_price_404_returns_http_error() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let err = super::fetch_pyth_price_with_url(&server.uri(), "feed-1", 60, 50)
            .await
            .unwrap_err();
        assert!(matches!(err, PythPriceError::HttpError { status: 404, .. }));
    }

    #[tokio::test]
    async fn fetch_pyth_price_500_returns_http_error() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = super::fetch_pyth_price_with_url(&server.uri(), "feed-1", 60, 50)
            .await
            .unwrap_err();
        assert!(matches!(err, PythPriceError::HttpError { status: 500, .. }));
    }

    // #532 — array branch must match the requested feed_id, not blindly pop
    #[tokio::test]
    async fn hermes_array_with_wrong_feed_id_returns_missing_feed_id_error() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let publish = recent_publish_time();
        // Array with a single feed whose id does NOT match the requested one
        let body = format!(
            r#"[{{"id":"other_feed_id","price":{{"price":"4500000000","expo":-8,"conf":"100000","publish_time":{}}}}}]"#,
            publish
        );

        Mock::given(method("GET"))
            .and(query_param("ids[]", "requested_feed_id"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let err = super::fetch_pyth_price_with_url(&server.uri(), "requested_feed_id", 60, 50)
            .await
            .unwrap_err();
        assert!(
            matches!(err, PythPriceError::MissingFeedId(ref id) if id == "requested_feed_id"),
            "expected MissingFeedId for requested_feed_id, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn hermes_array_with_matching_feed_id_returns_correct_feed() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let publish = recent_publish_time();
        // Array with two feeds; only feed_b matches the requested id
        let body = format!(
            r#"[{{"id":"feed_a","price":{{"price":"1000000000","expo":-8,"conf":"100000","publish_time":{}}}}},{{"id":"feed_b","price":{{"price":"4500000000","expo":-8,"conf":"100000","publish_time":{}}}}}]"#,
            publish, publish
        );

        Mock::given(method("GET"))
            .and(query_param("ids[]", "feed_b"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let result = super::fetch_pyth_price_with_url(&server.uri(), "feed_b", 60, 50)
            .await
            .unwrap();
        assert_eq!(result, 45 * FLOAT_PRECISION);
    }

    // #365 — normalize_pyth_price("4500000000", -8) must equal 45 * FLOAT_PRECISION
    // exponent_diff = 30 + (-8) = 22 → 4_500_000_000 * 10^22 = 45 * 10^30
    #[test]
    fn normalize_pyth_price_negative_exponent_gives_correct_value() {
        let result = normalize_pyth_price("4500000000", -8).unwrap();
        assert_eq!(result, 45 * FLOAT_PRECISION);
    }

    // #367 — validate_pyth_price with a fresh publish_time must return Ok(price)
    #[test]
    fn validate_pyth_price_accepts_fresh_price() {
        let data = PythPriceData {
            price: "4500000000".to_string(),
            conf: None,
            expo: -8,
            publish_time: Some(1_000),
        };
        // now=1_010, age=10 < stale_after=60 → accepted
        let price = validate_pyth_price(&data, 1_010, 60, 100).unwrap();
        assert_eq!(price, 45 * FLOAT_PRECISION);
    }

    // #366 — normalize_pyth_price rejects a positive exponent with PriceParseError
    #[test]
    fn issue_366_normalize_rejects_positive_exponent() {
        // Any exponent > 0 is outside the accepted range (-30..=0)
        // and must return Err(PriceParseError).
        let err = normalize_pyth_price("100000000", 1).unwrap_err();
        assert!(
            matches!(err, PythPriceError::PriceParseError(_)),
            "expected PriceParseError for exponent 1, got {:?}",
            err
        );

        let err2 = normalize_pyth_price("1", 30).unwrap_err();
        assert!(
            matches!(err2, PythPriceError::PriceParseError(_)),
            "expected PriceParseError for exponent 30, got {:?}",
            err2
        );
    }

    // #368 — validate_pyth_price returns StalePrice when age > stale_after_seconds
    #[test]
    fn issue_368_validate_rejects_stale_price() {
        // publish_time=1_000, now=1_500, age=500 > stale_after=60
        let data = PythPriceData {
            price: "4500000000".to_string(),
            conf: None,
            expo: -8,
            publish_time: Some(1_000),
        };
        let err = validate_pyth_price(&data, 1_500, 60, 100).unwrap_err();
        assert!(
            matches!(
                err,
                PythPriceError::StalePrice {
                    age_seconds,
                    max_age_seconds: 60,
                } if age_seconds == 500
            ),
            "expected StalePrice with age_seconds=500 and max_age_seconds=60, got {:?}",
            err
        );
    }

    // #369 — validate_pyth_price returns ConfidenceTooWide when confidence_bps > max_bps
    #[test]
    fn issue_369_validate_rejects_wide_confidence() {
        // price=100_000_000, conf=10_000_000 → confidence_bps = 1_000 bps; max_bps=50 → rejected
        let data = PythPriceData {
            price: "100000000".to_string(),
            conf: Some("10000000".to_string()),
            expo: -8,
            publish_time: Some(1_000),
        };
        let err = validate_pyth_price(&data, 1_010, 60, 50).unwrap_err();
        assert!(
            matches!(err, PythPriceError::ConfidenceTooWide { max_bps: 50, .. }),
            "expected ConfidenceTooWide with max_bps=50, got {:?}",
            err
        );
    }
}
