/// Constant-time byte slice comparison to avoid timing side-channels.
///
/// Returns `true` only when `left` and `right` have identical length and
/// contents.  Both branches of every byte comparison are always evaluated so
/// the execution time does not reveal the position of the first differing byte.
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    // XOR the lengths — non-zero when they differ.
    let mut diff = left.len() ^ right.len();

    for i in 0..max_len {
        let a = left.get(i).copied().unwrap_or(0);
        let b = right.get(i).copied().unwrap_or(0);
        diff |= (a ^ b) as usize;
    }

    diff == 0
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
