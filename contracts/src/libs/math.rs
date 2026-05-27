use soroban_sdk::{contracttype, Env, I256};

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MathError {
    Underflow = 1,
}

pub const FLOAT_PRECISION: i128 = 1_000_000_000_000_000_000_000_000_000_000;

pub fn safe_sub_u128(a: u128, b: u128) -> Result<u128, MathError> {
    a.checked_sub(b).ok_or(MathError::Underflow)
}

pub fn checked_sub_u128(a: u128, b: u128) -> u128 {
    a.checked_sub(b).expect("u128 underflow")
}

pub fn mul_div_wide(env: &Env, a: i128, b: i128, denominator: i128) -> i128 {
    let a_256 = I256::from_i128(env, a);
    let b_256 = I256::from_i128(env, b);
    let denom_256 = I256::from_i128(env, denominator);

    let prod = a_256.mul(&b_256);
    let res = prod.div(&denom_256);
    res.to_i128().expect("mul_div_wide result overflow i128")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_sub() {
        assert_eq!(safe_sub_u128(10, 5), Ok(5));
        assert_eq!(safe_sub_u128(10, 10), Ok(0));
        assert_eq!(safe_sub_u128(5, 10), Err(MathError::Underflow));
    }

    #[test]
    fn test_checked_sub_ok() {
        assert_eq!(checked_sub_u128(10, 5), 5);
        assert_eq!(checked_sub_u128(10, 10), 0);
    }

    #[test]
    #[should_panic(expected = "u128 underflow")]
    fn test_checked_sub_panic() {
        checked_sub_u128(5, 10);
    }

    #[test]
    fn test_mul_div_wide_float_precision_1() {
        let env = Env::default();
        let a = 2 * FLOAT_PRECISION;
        let b = 3 * FLOAT_PRECISION;
        let denom = FLOAT_PRECISION;
        let expected = 6 * FLOAT_PRECISION;
        assert_eq!(mul_div_wide(&env, a, b, denom), expected);
    }

    #[test]
    fn test_mul_div_wide_float_precision_2() {
        let env = Env::default();
        let a = i128::MAX;
        let b = FLOAT_PRECISION;
        let denom = FLOAT_PRECISION;
        assert_eq!(mul_div_wide(&env, a, b, denom), i128::MAX);
    }

    #[test]
    fn test_mul_div_wide_float_precision_3() {
        let env = Env::default();
        let a = i128::MAX;
        let b = i128::MAX;
        let denom = i128::MAX;
        assert_eq!(mul_div_wide(&env, a, b, denom), i128::MAX);
    }

    #[test]
    fn test_mul_div_wide_float_precision_4() {
        let env = Env::default();
        let a = i128::MAX / 2;
        let b = 4;
        let denom = 2;
        assert_eq!(mul_div_wide(&env, a, b, denom), (i128::MAX / 2) * 2);
    }

    #[test]
    fn test_mul_div_wide_float_precision_5() {
        let env = Env::default();
        let val = 123456789012345678901234567890;
        let a = val;
        let b = FLOAT_PRECISION;
        let denom = FLOAT_PRECISION;
        assert_eq!(mul_div_wide(&env, a, b, denom), val);
    }
}

