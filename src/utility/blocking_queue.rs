//! A thread-safe, bounded blocking queue implementation for producer-consumer scenarios.
//!
//! This module provides [`BlockingQueue`], a FIFO (first-in, first-out) queue that:
//! - Has a fixed, user-defined capacity.
//! - Blocks producers (`push`) when the queue is full.
//! - Blocks consumers (`pop`) when the queue is empty.
//! - Supports graceful shutdown via the `close` method, after which no more items can be pushed,
//!   and consumers will receive `None` once the queue is drained.
//!
//! The queue is built using [`Mutex`] and [`Condvar`] from the standard library, ensuring
//! safe concurrent access across multiple threads. It is particularly useful in pipeline-style
//! applications such as file backup systems, where a scanner thread produces file metadata
//! and multiple copier threads consume it without overwhelming memory.
//!
//! # Thread Safety
//!
//! [`BlockingQueue`] is `Sync` and `Send`, and can be safely shared among multiple threads.
//!
//! # Performance Notes
//!
//! - The queue uses a [`VecDeque`] internally, which provides O(1) amortized push/pop at both ends.
//! - Blocking operations avoid busy-waiting by leveraging OS-level condition variables.
//! - Spurious wakeups are handled by re-checking queue state in loops.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

/// A thread-safe, bounded blocking queue.
///
/// This queue supports multiple concurrent producers and consumers.
/// It blocks on `push` when full and on `pop` when empty, enabling natural backpressure
/// in data pipelines. The queue can be explicitly closed to signal end-of-stream.
///
/// Internally, it uses a [`Mutex`] to protect shared state and two [`Condvar`]s
/// (`not_full` and `not_empty`) to coordinate between threads.
#[derive(Debug)]
pub struct BlockingQueue<T> {
    inner: Mutex<(VecDeque<T>, usize, bool)>,
    not_full: Condvar,
    not_empty: Condvar,
}

impl<T> BlockingQueue<T> {
    /// Creates a new bounded blocking queue with the given capacity.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "Capacity must be greater than zero");
        Self {
            inner: Mutex::new((VecDeque::with_capacity(capacity), capacity, false)),
            not_full: Condvar::new(),
            not_empty: Condvar::new(),
        }
    }

    /// Closes the queue, preventing further pushes.
    ///
    /// After calling `close()`:
    /// - Any subsequent call to `push` will return `Err(item)`.
    /// - Consumers calling `pop` will continue to receive remaining items.
    /// - Once the queue is empty, `pop` returns `None` to signal completion.
    ///
    /// This method is idempotent and safe to call multiple times.
    pub fn close(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.2 = true;
            self.not_empty.notify_all();
        }
    }

    /// Pushes an item into the queue, blocking if the queue is full.
    ///
    /// Returns `Ok(())` on success, `Err(item)` if the queue is closed.
    ///
    /// This method will block the current thread until space becomes available.
    /// It handles spurious wakeups internally.
    pub fn push(&self, item: T) -> Result<(), T> {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        if inner.2 {
            return Err(item);
        }

        // Wait until there's space
        while inner.0.len() >= inner.1 {
            inner = match self.not_full.wait(inner) {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if inner.0.len() < inner.1 {
                break;
            }
        }

        inner.0.push_back(item);
        self.not_empty.notify_one();
        Ok(())
    }

    /// Pops an item from the queue, blocking if the queue is empty.
    ///
    /// Returns `Some(item)` if an item is available.
    /// Returns `None` if the queue is **closed** and **empty**.
    ///
    /// This method blocks the current thread until an item is available or the queue is closed.
    /// Spurious wakeups are handled internally.
    pub fn pop(&self) -> Option<T> {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        // Wait until there's an item or the queue is closed
        while inner.0.is_empty() {
            if inner.2 {
                return None;
            }
            inner = match self.not_empty.wait(inner) {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if !inner.0.is_empty() {
                break;
            }
        }

        let item = inner.0.pop_front().expect("loop invariant: queue non-empty");
        self.not_full.notify_one();
        Some(item)
    }

    /// Returns the current number of elements in the queue.
    ///
    /// This value may be outdated immediately after retrieval due to concurrent operations.
    /// Useful for monitoring or debugging, not for synchronization logic.
    pub fn len(&self) -> usize {
        match self.inner.lock() {
            Ok(inner) => inner.0.len(),
            Err(poisoned) => poisoned.into_inner().0.len(),
        }
    }

    /// Returns the maximum capacity of the queue.
    ///
    /// This value never changes after construction.
    pub fn capacity(&self) -> usize {
        match self.inner.lock() {
            Ok(inner) => inner.1,
            Err(poisoned) => poisoned.into_inner().1,
        }
    }
}
