use std::future::Future;
use std::time::Duration;
use tokio::time::MissedTickBehavior;

/// Run `run_price_cycle` on every tick of a `tokio::interval`.
///
/// The interval period is `price_loop_interval`.  Missed ticks use
/// `MissedTickBehavior::Delay` so a slow cycle never causes back-to-back
/// executions.  The loop runs forever; callers that need cancellation should
/// wrap this in a `tokio::select!` with an external shutdown signal.
pub async fn run_price_loop<F, Fut>(price_loop_interval: Duration, mut run_price_cycle: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let mut interval = tokio::time::interval(price_loop_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        interval.tick().await;
        run_price_cycle().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Verify that run_price_loop calls run_price_cycle at least twice and
    /// respects MissedTickBehavior::Delay (no panics, counter increments).
    #[tokio::test]
    async fn test_run_price_loop_ticks() {
        let counter = Arc::new(Mutex::new(0u32));
        let counter_clone = Arc::clone(&counter);

        // Use a very short interval so the test completes quickly.
        let interval = Duration::from_millis(10);

        tokio::time::timeout(Duration::from_millis(55), async move {
            run_price_loop(interval, || {
                let c = Arc::clone(&counter_clone);
                async move {
                    let mut lock = c.lock().unwrap();
                    *lock += 1;
                }
            })
            .await;
        })
        .await
        .ok(); // timeout is expected — we just want a few ticks

        let count = *counter.lock().unwrap();
        assert!(count >= 2, "expected at least 2 ticks, got {count}");
    }
}
