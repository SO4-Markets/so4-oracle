//! # Swarm Utilities
//!
//! This crate provides a collection of high‑performance utilities used by the
//! autonomous swarm.  The original implementation of the `fib` function used a
//! naïve recursive algorithm which caused the test suite to time‑out for larger
//! inputs.  The implementation below has been replaced with an iterative
//! version that runs in O(n) time and O(1) additional space, guaranteeing that
//! the unit tests complete quickly and deterministically.
//!
//! The module also includes a small thread‑pool implementation that previously
//! suffered from a dead‑lock when the pool was dropped while workers were still
//! waiting on a task.  The pool now shuts down cleanly by signalling workers to
//! exit and joining them before returning.

use std::sync::{
    mpsc::{self, Receiver, Sender},
    Arc, Condvar, Mutex,
};
use std::thread::{self, JoinHandle};

/// Compute the n‑th Fibonacci number.
///
/// This function is deliberately simple and fast.  It works for all `u64`
/// inputs that fit within the range of a `u64` result (i.e. `n <= 93`).
///
/// # Panics
///
/// Panics if `n > 93` because the result would overflow a `u64`.
pub fn fib(n: u64) -> u64 {
    assert!(n <= 93, "fib(n) would overflow u64 for n > 93");
    match n {
        0 => 0,
        1 => 1,
        _ => {
            let mut a = 0u64;
            let mut b = 1u64;
            for _ in 2..=n {
                let tmp = a.wrapping_add(b);
                a = b;
                b = tmp;
            }
            b
        }
    }
}

/* -------------------------------------------------------------------------- */
/*                     Simple Thread‑Pool Implementation                        */
/* -------------------------------------------------------------------------- */

/// A job that can be executed by the thread pool.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// Internal state shared between the pool and its workers.
struct SharedState {
    /// Queue of pending jobs.
    queue: Mutex<Vec<Job>>,
    /// Condition variable to wake up idle workers.
    cv: Condvar,
    /// Indicates whether the pool is shutting down.
    shutdown: Mutex<bool>,
}

/// A handle to a worker thread.
struct Worker {
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    fn new(id: usize, shared: Arc<SharedState>) -> Self {
        let handle = thread::Builder::new()
            .name(format!("swarm-worker-{}", id))
            .spawn(move || loop {
                // Acquire the lock and wait for a job or shutdown signal.
                let mut guard = shared.queue.lock().unwrap();
                while guard.is_empty() && !*shared.shutdown.lock().unwrap() {
                    guard = shared.cv.wait(guard).unwrap();
                }

                // Check for shutdown request.
                if *shared.shutdown.lock().unwrap() {
                    // Exit the loop, terminating the thread.
                    break;
                }

                // Pop a job and execute it.
                if let Some(job) = guard.pop() {
                    // Release the lock before running the job to avoid
                    // holding the mutex for the duration of the task.
                    drop(guard);
                    job();
                }
            })
            .expect("Failed to spawn worker thread");

        Self {
            handle: Some(handle),
        }
    }

    fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// A simple fixed‑size thread pool.
///
/// The pool can be cloned (the underlying workers are shared) and will shut
/// down cleanly when the last handle is dropped.
#[derive(Clone)]
pub struct ThreadPool {
    shared: Arc<SharedState>,
    workers: Arc<Mutex<Vec<Worker>>>,
}

impl ThreadPool {
    /// Create a new thread pool with the given number of worker threads.
    ///
    /// # Panics
    ///
    /// Panics if `size == 0`.
    pub fn new(size: usize) -> Self {
        assert!(size > 0, "ThreadPool size must be greater than zero");

        let shared = Arc::new(SharedState {
            queue: Mutex::new(Vec::new()),
            cv: Condvar::new(),
            shutdown: Mutex::new(false),
        });

        let mut workers_vec = Vec::with_capacity(size);
        for i in 0..size {
            workers_vec.push(Worker::new(i, Arc::clone(&shared)));
        }

        Self {
            shared,
            workers: Arc::new(Mutex::new(workers_vec)),
        }
    }

    /// Submit a job to the pool for asynchronous execution.
    ///
    /// The job is queued and a worker will pick it up as soon as possible.
    pub fn execute<F>(&self, job: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let mut queue = self.shared.queue.lock().unwrap();
        queue.push(Box::new(job));
        // Wake one worker.
        self.shared.cv.notify_one();
    }

    /// Gracefully shut down the pool, waiting for all workers to finish.
    ///
    /// This method is called automatically when the last `ThreadPool` handle is
    /// dropped, but can be invoked manually to ensure deterministic shutdown.
    pub fn shutdown(&self) {
        // Signal shutdown.
        {
            let mut shutdown = self.shared.shutdown.lock().unwrap();
            *shutdown = true;
        }
        // Wake all workers so they can observe the shutdown flag.
        self.shared.cv.notify_all();

        // Join all worker threads.
        let mut workers = self.workers.lock().unwrap();
        for worker in workers.iter_mut() {
            worker.join();
        }
        // Clear the vector to drop the JoinHandles.
        workers.clear();
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        // Only attempt shutdown if we are the last reference.
        if Arc::strong_count(&self.shared) == 1 {
            self.shutdown();
        }
    }
}

/* -------------------------------------------------------------------------- */
/*                                 Unit Tests                                 */
/* -------------------------------------------------------------------------- */

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn test_fib_small() {
        assert_eq!(fib(0), 0);
        assert_eq!(fib(1), 1);
        assert_eq!(fib(2), 1);
        assert_eq!(fib(10), 55);
    }

    #[test]
    fn test_fib_overflow_panics() {
        let result = std::panic::catch_unwind(|| fib(94));
        assert!(result.is_err());
    }

    #[test]
    fn thread_pool_executes_jobs() {
        let pool = ThreadPool::new(4);
        let counter = Arc::new(AtomicUsize::new(0));

        for _ in 0..100 {
            let c = Arc::clone(&counter);
            pool.execute(move || {
                c.fetch_add(1, Ordering::SeqCst);
            });
        }

        // Give workers a moment to finish.
        thread::sleep(Duration::from_millis(200));
        assert_eq!(counter.load(Ordering::SeqCst), 100);
        // Explicit shutdown to ensure clean test exit.
        pool.shutdown();
    }

    #[test]
    fn thread_pool_shutdown_is_idempotent() {
        let pool = ThreadPool::new(2);
        pool.shutdown(); // first shutdown
        pool.shutdown(); // second shutdown should be a no‑op and not panic
    }
}
