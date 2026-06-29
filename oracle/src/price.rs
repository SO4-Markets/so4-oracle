/// Errors produced by price-source validation and aggregation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OracleError {
    /// A price of zero was supplied where a positive value is required.
    ZeroPrice,
    /// A negative price was supplied; oracle prices must be strictly positive.
    NegativePrice,
}

impl core::fmt::Display for OracleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            OracleError::ZeroPrice => write!(f, "price must be positive, got zero"),
            OracleError::NegativePrice => write!(f, "price must be positive, got negative value"),
        }
    }
}

/// A price source that always returns a single fixed value.
///
/// The value must be strictly positive (`> 0`). Attempting to construct a
/// `FixedSource` with zero or a negative value returns an [`OracleError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedSource {
    price: i128,
}

impl FixedSource {
    /// Create a `FixedSource` from a raw `i128` price.
    ///
    /// Returns `Err(OracleError::ZeroPrice)` when `price == 0`, and
    /// `Err(OracleError::NegativePrice)` when `price < 0`.
    pub fn new(price: i128) -> Result<Self, OracleError> {
        if price < 0 {
            return Err(OracleError::NegativePrice);
        }
        if price == 0 {
            return Err(OracleError::ZeroPrice);
        }
        Ok(Self { price })
    }

    /// Return the fixed price.
    pub fn price(&self) -> i128 {
        self.price
    }
}

impl std::str::FromStr for FixedSource {
    type Err = OracleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let price: i128 = s.trim().parse().map_err(|_| OracleError::ZeroPrice)?;
        Self::new(price)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    // ── FixedSource::new ───────────────────────────────────────────────────────

    #[test]
    fn fixed_source_rejects_zero() {
        assert_eq!(FixedSource::new(0), Err(OracleError::ZeroPrice));
    }

    #[test]
    fn fixed_source_rejects_negative_one() {
        assert_eq!(FixedSource::new(-1), Err(OracleError::NegativePrice));
    }

    #[test]
    fn fixed_source_rejects_large_negative() {
        assert_eq!(
            FixedSource::new(-1_000_000),
            Err(OracleError::NegativePrice)
        );
    }

    #[test]
    fn fixed_source_accepts_positive() {
        let src = FixedSource::new(1_000_000).unwrap();
        assert_eq!(src.price(), 1_000_000);
    }

    // ── FixedSource::from_str ─────────────────────────────────────────────────

    #[test]
    fn fixed_source_from_str_rejects_zero_string() {
        assert_eq!(FixedSource::from_str("0"), Err(OracleError::ZeroPrice));
    }

    #[test]
    fn fixed_source_from_str_rejects_negative_string() {
        assert_eq!(FixedSource::from_str("-1"), Err(OracleError::NegativePrice));
    }

    #[test]
    fn fixed_source_from_str_accepts_valid_string() {
        let src = FixedSource::from_str("500").unwrap();
        assert_eq!(src.price(), 500);
    }

    #[test]
    fn fixed_source_from_str_handles_whitespace() {
        let src = FixedSource::from_str("  42  ").unwrap();
        assert_eq!(src.price(), 42);
    }
}
