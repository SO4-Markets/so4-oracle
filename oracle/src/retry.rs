/// Applies multiplicative jitter to a computed backoff delay, scaling it by a
/// random factor in `[0.5, 1.0)` (resolves #583).
///
/// Concurrent retry loops (e.g. multiple keeper cycles or price-source
/// fetches) that hit a transient outage at the same wall-clock moment would
/// otherwise back off on the exact same schedule and retry in lockstep —
/// the classic thundering-herd pattern. Jittering only the sleep duration,
/// rather than the growth trajectory itself, keeps the delay's order of
/// magnitude intact while still spreading concurrent retries apart.
pub fn jitter(delay_ms: u64) -> u64 {
    let factor = 0.5 + rand::random::<f64>() * 0.5;
    jitter_with(delay_ms, factor)
}

/// Apply a caller-supplied factor in `[0.5, 1.0)` to a backoff delay.
///
/// Kept separate from [`jitter`] so the contract can be property-tested with
/// deterministic factors instead of relying on random draws. The result is
/// never larger than `delay_ms` (so a configured cap still holds after
/// jittering) and never zero for a non-zero input (so a retry loop cannot
/// degenerate into a busy loop).
fn jitter_with(delay_ms: u64, factor: f64) -> u64 {
    if delay_ms == 0 {
        return 0;
    }
    let factor = factor.clamp(0.5, 1.0);
    let scaled = (delay_ms as f64 * factor) as u64;
    scaled.clamp(1, delay_ms)
}

/// Maximum backoff delay cap in milliseconds (30 seconds).
/// Used across all retry_with_backoff call sites to ensure a consistent cap.
pub const MAX_BACKOFF_DELAY_MS: u64 = 30_000;

/// Retry an async fallible closure with exponential backoff (resolves #356, #585).
///
/// Doubles the delay after every failure, starting at `base_delay_ms`, and
/// caps the delay at `max_delay_ms` so that runaway growth is impossible
/// regardless of how many attempts the caller configures.  Mirrors the
/// explicit `(backoff_ms * 2).min(MAX_BACKOFF_DELAY_MS)` cap already present in
/// `poll_until_confirmed` in `submit.rs`. Each sleep is jittered (#583) so
/// concurrent callers don't retry in lockstep.
///
/// Returns `Ok(T)` on the first success, or the last `Err(E)` after all
/// attempts are exhausted.
///
/// If `E` implements `Retryable` and returns `false` for `is_retryable()`,
/// the error is returned immediately without further attempts.
pub async fn retry_with_backoff<F, Fut, T, E>(
    mut f: F,
    max_attempts: u32,
    base_delay_ms: u64,
    max_delay_ms: u64,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Debug + Retryable,
{
    assert!(
        max_attempts > 0,
        "retry_with_backoff requires max_attempts >= 1"
    );
    let mut delay_ms = base_delay_ms.min(max_delay_ms);
    let mut last_err: Option<E> = None;

    for attempt in 1..=max_attempts {
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                let retryable = e.is_retryable();
                log_retry_failure(attempt, max_attempts, &e, retryable);

                if !retryable {
                    tracing::info!("non-retryable error encountered, aborting retry loop");
                    return Err(e);
                }

                last_err = Some(e);
                if attempt < max_attempts {
                    sleep_ms(jitter(delay_ms)).await;
                    delay_ms = (delay_ms * 2).min(max_delay_ms);
                }
            }
        }
    }

    Err(last_err.expect("loop exhausted with max_attempts >= 1"))
}

/// Trait to determine if an error should be retried.
///
/// `is_retryable` is deliberately required, with no default and no impls for
/// `String`/`&str` (#1227): a blanket "retry everything" default meant that
/// deterministic failures (contract reverts, type mismatches, unparseable
/// responses) were retried with backoff on every call, multiplying RPC load
/// and log noise for errors that can never succeed. Every error type passed to
/// [`retry_with_backoff`] must now say which of its variants are transient.
pub trait Retryable {
    /// Returns true if this error represents a transient condition that may succeed on retry.
    /// Returns false for permanent failures like config errors or parse errors.
    fn is_retryable(&self) -> bool;
}

/// Shared HTTP status classification for every `Retryable` impl that wraps an
/// HTTP response (#1448), so the RPC client and the price sources cannot
/// drift apart again: request timeout (408), too early (425), rate limited
/// (429), and every 5xx are transient; every other status is permanent.
pub fn is_transient_http_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429) || status >= 500
}

fn log_retry_failure<E: std::fmt::Debug>(
    attempt: u32,
    max_attempts: u32,
    error: &E,
    retryable: bool,
) {
    tracing::warn!(
        event = "retry_attempt_failed",
        attempt,
        max_attempts,
        retryable,
        error = ?error,
        "retry attempt failed"
    );
}

/// Async sleep for native tokio runtime.
async fn sleep_ms(ms: u64) {
    if ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test-only: these tests exercise the backoff schedule with plain `&str`
    // errors, so treat them as transient here. Production code has no
    // blanket impl and must classify its errors (#1227).
    impl Retryable for &str {
        fn is_retryable(&self) -> bool {
            true
        }
    }

    /// #1227 — the opposite classification: a deterministic failure that
    /// must abort on the first attempt.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TerminalError(u32);

    impl Retryable for TerminalError {
        fn is_retryable(&self) -> bool {
            false
        }
    }

    use std::cell::Cell;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    #[test]
    fn transient_http_statuses() {
        for status in [408, 425, 429, 500, 502, 503, 504, 599] {
            assert!(is_transient_http_status(status), "{status} should retry");
        }
        for status in [200, 301, 400, 401, 403, 404, 409, 422, 499] {
            assert!(
                !is_transient_http_status(status),
                "{status} should not retry"
            );
        }
    }

    #[test]
    fn succeeds_on_third_attempt() {
        let call_count = Rc::new(Cell::new(0u32));

        let result: Result<u32, &'static str> = block_on(async {
            let count = Rc::clone(&call_count);
            retry_with_backoff(
                || {
                    let count = Rc::clone(&count);
                    async move {
                        let n = count.get() + 1;
                        count.set(n);
                        if n < 3 {
                            Err("transient")
                        } else {
                            Ok(42u32)
                        }
                    }
                },
                3,
                0,
                MAX_BACKOFF_DELAY_MS,
            )
            .await
        });

        assert_eq!(result, Ok(42));
        assert_eq!(call_count.get(), 3);
    }

    #[test]
    fn exhausts_all_attempts_and_returns_last_error() {
        let call_count = Rc::new(Cell::new(0u32));

        let result: Result<u32, &'static str> = block_on(async {
            let count = Rc::clone(&call_count);
            retry_with_backoff(
                || {
                    let count = Rc::clone(&count);
                    async move {
                        count.set(count.get() + 1);
                        Err("always fails")
                    }
                },
                3,
                0,
                MAX_BACKOFF_DELAY_MS,
            )
            .await
        });

        assert_eq!(result, Err("always fails"));
        assert_eq!(call_count.get(), 3);
    }

    /// The `!retryable` branch decides whether a deterministic contract revert
    /// is retried forever or dropped at once. Without this test every case in
    /// this module uses `&str`, which is retryable, so the branch is uncovered.
    #[test]
    fn non_retryable_error_calls_once_and_returns_the_original_error() {
        let call_count = Rc::new(Cell::new(0u32));
        let original = TerminalError(42);

        let result: Result<(), TerminalError> = block_on(async {
            let count = Rc::clone(&call_count);
            let error = original.clone();
            retry_with_backoff(
                || {
                    let count = Rc::clone(&count);
                    let error = error.clone();
                    async move {
                        count.set(count.get() + 1);
                        Err(error)
                    }
                },
                5, // the budget would be consumed if the flag were ignored
                0,
                MAX_BACKOFF_DELAY_MS,
            )
            .await
        });

        assert_eq!(
            result,
            Err(original),
            "the original error value must be returned"
        );
        assert_eq!(
            call_count.get(),
            1,
            "a non-retryable error must abort immediately"
        );
    }

    #[test]
    fn panics_when_max_attempts_is_zero() {
        let result = std::panic::catch_unwind(|| {
            block_on(async {
                retry_with_backoff(
                    || async { Ok::<u32, &'static str>(1) },
                    0,
                    100,
                    MAX_BACKOFF_DELAY_MS,
                )
                .await
            })
        });
        assert!(result.is_err());
    }

    #[test]
    fn succeeds_on_first_attempt_calls_once() {
        let call_count = Rc::new(Cell::new(0u32));

        let result: Result<u32, &'static str> = block_on(async {
            let count = Rc::clone(&call_count);
            retry_with_backoff(
                || {
                    let count = Rc::clone(&count);
                    async move {
                        count.set(count.get() + 1);
                        Ok(99u32)
                    }
                },
                3,
                0,
                MAX_BACKOFF_DELAY_MS,
            )
            .await
        });

        assert_eq!(result, Ok(99));
        assert_eq!(call_count.get(), 1);
    }

    #[tokio::test]
    async fn max_delay_cap_is_respected() {
        tokio::time::pause();
        let call_count = Rc::new(Cell::new(0u32));
        let attempts = Rc::new(RefCell::new(Vec::new()));

        // base_delay_ms (200) > max_delay_ms (50): the first sleep must be
        // clamped to the cap. With the clock paused, the gaps between the
        // recorded attempt timestamps ARE the sleep durations, so the cap is
        // verified directly instead of merely assumed.
        let count = Rc::clone(&call_count);
        let stamps = Rc::clone(&attempts);
        let result: Result<u32, &'static str> = retry_with_backoff(
            || {
                let count = Rc::clone(&count);
                let stamps = Rc::clone(&stamps);
                async move {
                    stamps.borrow_mut().push(tokio::time::Instant::now());
                    let n = count.get() + 1;
                    count.set(n);
                    if n < 3 {
                        Err("transient")
                    } else {
                        Ok(7u32)
                    }
                }
            },
            5,
            200, // base_delay intentionally larger than the cap
            50,  // max_delay cap
        )
        .await;

        assert_eq!(result, Ok(7));
        assert_eq!(call_count.get(), 3);

        let stamps = attempts.borrow();
        assert_eq!(stamps.len(), 3);
        let gaps: Vec<_> = stamps
            .windows(2)
            .map(|pair| pair[1].duration_since(pair[0]).as_millis())
            .collect();
        assert_eq!(gaps.len(), 2, "expected exactly two inter-attempt sleeps");
        for gap in &gaps {
            assert!(
                (25..=50).contains(gap),
                "jittered sleep {gap}ms escaped the 50 ms cap (expected [25, 50])"
            );
        }
    }

    #[tokio::test]
    async fn uncapped_backoff_grows_exponentially() {
        tokio::time::pause();
        let call_count = Rc::new(Cell::new(0u32));
        let attempts = Rc::new(RefCell::new(Vec::new()));

        let count = Rc::clone(&call_count);
        let stamps = Rc::clone(&attempts);
        let result: Result<u32, &'static str> = retry_with_backoff(
            || {
                let count = Rc::clone(&count);
                let stamps = Rc::clone(&stamps);
                async move {
                    stamps.borrow_mut().push(tokio::time::Instant::now());
                    let n = count.get() + 1;
                    count.set(n);
                    if n < 4 {
                        Err("transient")
                    } else {
                        Ok(1u32)
                    }
                }
            },
            4,
            100,
            60_000, // effectively uncapped
        )
        .await;

        assert_eq!(result, Ok(1));

        let stamps = attempts.borrow();
        let gaps: Vec<_> = stamps
            .windows(2)
            .map(|pair| pair[1].duration_since(pair[0]).as_millis())
            .collect();
        // Uncapped growth is 100 → 200 → 400 ms, each jittered by [0.5, 1).
        for (gap, (low, high)) in gaps.iter().zip([(50, 100), (100, 200), (200, 400)]) {
            assert!(
                (low..=high).contains(gap),
                "sleep {gap}ms outside expected [{low}, {high}]"
            );
        }
    }

    #[test]
    fn jitter_never_exceeds_input_or_becomes_zero() {
        for delay in [0u64, 1, 2, 3, 25, 50, 200, 1_000, u64::MAX / 2] {
            for _ in 0..64 {
                let j = jitter(delay);
                assert!(j <= delay, "jitter({delay}) = {j} exceeded input");
                if delay > 0 {
                    assert!(j >= 1, "jitter({delay}) = 0 would busy-loop");
                }
            }
        }
    }

    #[test]
    fn jitter_with_honours_injected_factor_and_bounds() {
        assert_eq!(jitter_with(0, 0.7), 0);
        assert_eq!(
            jitter_with(1, 0.5),
            1,
            "must not return 0 for a positive delay"
        );
        assert_eq!(jitter_with(100, 0.5), 50);
        assert_eq!(jitter_with(100, 1.0), 100);
        assert_eq!(jitter_with(200, 2.0), 200, "factor clamped to <= 1");
        assert_eq!(jitter_with(200, 0.1), 100, "factor clamped to >= 0.5");
    }
}
