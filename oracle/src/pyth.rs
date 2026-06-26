use serde::Deserialize;

pub const PYTH_HERMES_URL: &str = "https://hermes.pyth.network/api/latest_price_feeds";
pub const FLOAT_PRECISION: i128 = 1_000_000_000_000_000_000_000_000_000_000;

#[derive(Debug, Clone, PartialEq)]
pub enum PythPriceError {
    NetworkError(String),
    HttpError(u16),
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
    InvalidPublishTime(i64),
}

#[derive(Debug, Deserialize)]
pub struct PythPrice {
    pub price: PythPriceData,
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct PythPriceData {
    pub price: String,
    #[serde(default)]
    pub conf: Option<String>,
    pub expo: i32,
    #[serde(default)]
    pub publish_time: Option<i64>,
}

#[derive(Debug, Deserialize)]
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
    if !(-30..=0).contains(&exponent) {
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

    let exponent_diff = 30 + exponent;

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
        .ok_or(PythPriceError::InvalidPublishTime(-1))?;
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

    if let Some(conf) = &data.conf {
        if price <= 0 {
            return Err(PythPriceError::PriceParseError(
                "price must be greater than zero".to_string(),
            ));
        }
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

pub async fn fetch_pyth_price(
    feed_id: &str,
    stale_after_seconds: u64,
    max_confidence_bps: u32,
) -> Result<i128, PythPriceError> {
    let url_string = format!("{}?ids[]={}", PYTH_HERMES_URL, feed_id);

    let response = crate::http::client()
        .get(&url_string)
        .send()
        .await
        .map_err(|err| PythPriceError::NetworkError(err.to_string()))?;

    let status = response.status().as_u16();
    if status != 200 {
        return Err(PythPriceError::HttpError(status));
    }

    let body = response
        .text()
        .await
        .map_err(|err| PythPriceError::NetworkError(err.to_string()))?;

    let response: HermesResponse =
        serde_json::from_str(&body).map_err(|err| PythPriceError::JsonError(err.to_string()))?;
    let feed = match response {
        HermesResponse::Array(mut feeds) => feeds
            .pop()
            .ok_or_else(|| PythPriceError::MissingFeedId(feed_id.to_string()))?,
        HermesResponse::Wrapped(wrapped) => wrapped.data,
    };

    validate_pyth_price(
        &feed.price,
        current_timestamp_secs(),
        stale_after_seconds,
        max_confidence_bps,
    )
}

fn current_timestamp_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
        assert_eq!(err, PythPriceError::InvalidPublishTime(-1));
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

    // #349 — fetch_pyth_price handles both array and wrapped Hermes response formats

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
}
