/// Retry an async fallible closure with exponential backoff.
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
    let mut delay_ms = base_delay_ms;
    let mut last_err: Option<E> = None;

    for attempt in 1..=max_attempts {
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                worker::console_log!(
                    "[retry] attempt {attempt}/{max_attempts} failed: {e:?}"
                );
                last_err = Some(e);
                if attempt < max_attempts {
                    sleep_ms(delay_ms).await;
                    delay_ms *= 2;
                }
            }
        }
    }

    Err(last_err.expect("max_attempts must be > 0"))
}

/// Async sleep.  On WASM (Cloudflare Workers) this uses JS `setTimeout`;
/// on native targets (unit tests) it is a no-op so tests run without delay.
async fn sleep_ms(ms: u64) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::prelude::*;
        let promise = js_sys::Promise::new(&mut |resolve, _| {
            web_sys::window()
                .expect("no window")
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32)
                .unwrap();
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }
    let _ = ms;
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
                        if n < 3 { Err("transient") } else { Ok(42u32) }
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
