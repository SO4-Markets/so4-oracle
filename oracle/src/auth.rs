use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Constant-time byte slice comparison to avoid timing side-channels.
///
/// Returns `true` only when `left` and `right` have identical length and
/// contents. Both sides are hashed to fixed-size SHA-256 digests first, so the
/// comparison does the same work whatever the supplied token's length, and the
/// digests are compared with `subtle::ConstantTimeEq`, whose constant-time
/// behaviour is maintained against compiler optimisation rather than left to
/// a hand-rolled loop (#1444).
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let left = Sha256::digest(left);
    let right = Sha256::digest(right);
    left.as_slice().ct_eq(right.as_slice()).into()
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    /// #342 — equal slices return true.
    #[test]
    fn constant_time_eq_equal_returns_true() {
        assert!(constant_time_eq(b"secret", b"secret"));
    }

    /// #342 — different content returns false.
    #[test]
    fn constant_time_eq_different_content_returns_false() {
        assert!(!constant_time_eq(b"secret", b"Secret"));
        assert!(!constant_time_eq(b"abc", b"xyz"));
    }

    /// #342 — different lengths return false even with matching prefix.
    #[test]
    fn constant_time_eq_different_length_returns_false() {
        assert!(!constant_time_eq(b"secret", b"secret2"));
        assert!(!constant_time_eq(b"a", b""));
    }

    #[test]
    fn constant_time_eq_empty_slices_are_equal() {
        assert!(constant_time_eq(b"", b""));
    }
}
