/// Retry an async fallible closure with exponential backoff (resolves #356).
///
/// Doubles the delay after every failure, starting at `base_delay_ms`.
/// Returns `Ok(T)` on the first success, or the last `Err(E)` after all
/// attempts are exhausted.
pub async fn retry_with_backoff<F, Fut, T, E>(
    mut f: F,
    max_attempts: u32,
    base_delay_ms: u64,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Debug,
{
    assert!(
        max_attempts > 0,
        "retry_with_backoff requires max_attempts >= 1"
    );
    let mut delay_ms = base_delay_ms;
    let mut last_err: Option<E> = None;

    for attempt in 1..=max_attempts {
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                log_retry_failure(attempt, max_attempts, &e);
                last_err = Some(e);
                if attempt < max_attempts {
                    sleep_ms(delay_ms).await;
                    delay_ms *= 2;
                }
            }
        }
    }

    Err(last_err.expect("loop exhausted with max_attempts >= 1"))
}

fn log_retry_failure<E: std::fmt::Debug>(attempt: u32, max_attempts: u32, error: &E) {
    tracing::warn!(attempt, max_attempts, error = ?error, "retry attempt failed");
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
                retry_with_backoff(|| async { Ok::<u32, &'static str>(1) }, 0, 100).await
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
            )
            .await
        });

        assert_eq!(result, Ok(99));
        assert_eq!(call_count.get(), 1);
    }
}
