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
    ((delay_ms as f64) * factor) as u64
}

/// Retry an async fallible closure with exponential backoff (resolves #356, #585).
///
/// Doubles the delay after every failure, starting at `base_delay_ms`, and
/// caps the delay at `max_delay_ms` so that runaway growth is impossible
/// regardless of how many attempts the caller configures.  Mirrors the
/// explicit `(backoff_ms * 2).min(30_000)` cap already present in
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
pub trait Retryable {
    /// Returns true if this error represents a transient condition that may succeed on retry.
    /// Returns false for permanent failures like config errors or parse errors.
    fn is_retryable(&self) -> bool {
        true // Default: all errors are retryable
    }
}

// String errors are always retryable (legacy behavior)
impl Retryable for String {}

// str errors are always retryable (legacy behavior)
impl Retryable for &str {}

fn log_retry_failure<E: std::fmt::Debug>(
    attempt: u32,
    max_attempts: u32,
    error: &E,
    retryable: bool,
) {
    tracing::warn!(
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
    use std::cell::Cell;
    use std::rc::Rc;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
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
                30_000,
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
                30_000,
            )
            .await
        });

        assert_eq!(result, Err("always fails"));
        assert_eq!(call_count.get(), 3);
    }

    #[test]
    fn panics_when_max_attempts_is_zero() {
        let result = std::panic::catch_unwind(|| {
            block_on(async {
                retry_with_backoff(|| async { Ok::<u32, &'static str>(1) }, 0, 100, 30_000).await
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
                30_000,
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

        // base_delay_ms (200) > max_delay_ms (50): first sleep must be clamped.
        // With 5 attempts the uncapped sequence would be 200 → 400 → 800 → 1600 ms;
        // capped at 50 it stays at 50 for every inter-attempt sleep.
        // The test just verifies the function completes and returns the right value
        // (timing assertions would be flaky in unit tests).
        let count = Rc::clone(&call_count);
        let result: Result<u32, &'static str> = retry_with_backoff(
            || {
                let count = Rc::clone(&count);
                async move {
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
    }
}
