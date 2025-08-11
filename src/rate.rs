//! Token bucket rate limiter for bytes transfer rate limitation.

#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_truncation)]

use std::cmp::min;
use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::{fmt, io};

use crossbeam_utils::CachePadded;
use tokio::time::Instant;

const TOKIO_TIMER_MIN_DUR: Duration = Duration::from_millis(1);

pub(crate) const RATE_LIMITER_ENABLED: bool = true;
pub(crate) const RATE_LIMITER_DISABLED: bool = false;

#[derive(Debug, Clone)]
/// Bytes transfer rate limitation, `B/s`.
///
/// The limitation can be shared by multiple connections, see
/// [`RateLimit::new_shared_by`] or [`RateLimit::set_share_by`].
///
/// # Notes
///
/// - (WIP) Not so that accurate, as Tokio's timer resolution is limited to 1ms.
pub struct RateLimit {
    total: CachePadded<Arc<AtomicU64>>,
    shared_by: CachePadded<Arc<AtomicU16>>,
}

impl RateLimit {
    const DISABLED: u64 = 0;

    /// Create a new [`RateLimit`].
    #[must_use]
    pub fn new(limit: NonZeroU64) -> Self {
        Self {
            total: CachePadded::new(Arc::new(AtomicU64::new(limit.get()))),
            shared_by: CachePadded::new(Arc::new(AtomicU16::new(1))),
        }
    }

    /// Create a new [`RateLimit`] that is disabled (total = 0).
    #[must_use]
    pub fn new_disabled() -> Self {
        Self {
            total: CachePadded::new(Arc::new(AtomicU64::new(Self::DISABLED))),
            shared_by: CachePadded::new(Arc::new(AtomicU16::new(1))),
        }
    }

    /// Create a new [`RateLimit`] that is shared by `N` instances.
    ///
    /// # Panics
    ///
    /// If `N` is 0.
    #[must_use]
    pub fn new_shared_by<const N: u16>(limit: NonZeroU64) -> Self {
        Self {
            total: CachePadded::new(Arc::new(AtomicU64::new(limit.get()))),
            shared_by: CachePadded::new(Arc::new(AtomicU16::new(
                NonZeroU16::new(N).expect("`shared_by cannot be 0`").get(),
            ))),
        }
    }

    /// Get the current limit for single connection.
    #[must_use]
    pub fn current(&self) -> Option<NonZeroU64> {
        let total = self.total.load(Ordering::Relaxed);

        if total == 0 {
            None
        } else {
            let shared_by = self.shared_by.load(Ordering::Relaxed);

            NonZeroU64::new((total + u64::from(shared_by)) / u64::from(shared_by))
        }
    }

    /// Disable the rate limit (by set to 0).
    pub fn set_disable(&self) {
        self.total.store(0, Ordering::Release);
    }

    /// Set the total rate limit.
    pub fn set_total(&self, limit: NonZeroU64) {
        self.total.store(limit.get(), Ordering::Release);
    }

    /// Update the number of shared instances.
    pub fn set_share_by(&self, shared_by: NonZeroU16) {
        self.shared_by.store(shared_by.get(), Ordering::Release);
    }

    /// Increase the number of shared instances by `N`.
    ///
    /// # Panics
    ///
    /// If `N` is 0.
    pub fn inc_shared_by_n<const N: u16>(&self) {
        self.inc_shared_by({
            NonZeroU16::new(N).expect("`inc_shared_by_n` cannot be called with 0")
        });
    }

    /// Increase the number of shared instances.
    pub fn inc_shared_by(&self, inc: NonZeroU16) {
        let _ = self
            .shared_by
            .fetch_update(Ordering::Release, Ordering::Acquire, |current| {
                Some(current.saturating_add(inc.get()))
            });
    }

    /// Decrease the number of shared instances by `N`.
    ///
    /// ## Panics
    ///
    /// If `N` is 0.
    ///
    /// ## Errors
    ///
    /// See [`dec_shared_by`](Self::dec_shared_by).
    pub fn dec_shared_by_n<const N: u16>(&self) -> io::Result<()> {
        self.dec_shared_by({
            NonZeroU16::new(N).expect("`dec_shared_by_n` cannot be called with 0")
        })
    }

    /// Decrease the number of shared instances.
    ///
    /// ## Errors
    ///
    /// Cannot decrease `shared_by` to 0.
    pub fn dec_shared_by(&self, dec: NonZeroU16) -> io::Result<()> {
        #[allow(clippy::redundant_closure_for_method_calls)]
        self.shared_by
            .fetch_update(Ordering::Release, Ordering::Acquire, |shared_by| {
                shared_by
                    .checked_sub(dec.get())
                    .and_then(NonZeroU16::new)
                    .map(|s| s.get())
            })
            .map(|_| ())
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot decrease `shared_by` to 0",
                )
            })
    }

    /// Clone the `RateLimit` with the same total limit, and increase the shared
    /// instances count by 1.
    #[must_use]
    pub fn clone_shared(&self) -> Self {
        let new_limit = self.clone();
        new_limit.inc_shared_by_n::<1>();
        new_limit
    }
}

pub(crate) struct RateLimiter<const ENABLED: bool> {
    /// Bytes transfer rate limitation, `B/s`.
    limit: RateLimit,

    /// Available tokens (in Bytes).
    tokens: Option<f64>,

    /// The last time the tokens were updated.
    last_updated: Option<Instant>,
}

impl<const ENABLED: bool> fmt::Debug for RateLimiter<ENABLED> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if ENABLED {
            f.debug_struct("RateLimiter")
                .field("enabled", &ENABLED)
                .field("limit", &self.limit)
                .field("tokens", &self.tokens)
                .field(
                    "since_last_updated",
                    &self.last_updated.map(|i| i.elapsed()),
                )
                .finish()
        } else {
            f.debug_struct("RateLimiter")
                .field("enabled", &ENABLED)
                .finish()
        }
    }
}

/// Rate limit result.
pub(crate) enum RateLimitResult {
    /// The requested number of bytes is accepted.
    Accepted,

    /// The requested number of bytes is throttled, should stop reading for the
    /// specified duration.
    Throttled { now: Instant, dur: Duration },
}

impl fmt::Debug for RateLimitResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RateLimitResult::Accepted => f.write_str("Accepted"),
            RateLimitResult::Throttled { dur, .. } => {
                f.debug_tuple("Throttled").field(dur).finish()
            }
        }
    }
}

impl RateLimiter<RATE_LIMITER_DISABLED> {
    /// Create a new rate limiter that is disabled.
    pub(crate) fn empty() -> Self {
        Self::new(RateLimit::new_disabled())
    }
}

impl<const ENABLED: bool> RateLimiter<ENABLED> {
    /// Create a new rate limiter with the specified [`RateLimit`].
    pub(crate) const fn new(limit: RateLimit) -> Self {
        Self {
            limit,
            tokens: None,
            last_updated: None,
        }
    }

    #[inline]
    #[cfg_attr(
        any(
            feature = "feat-tracing-trace",
            all(debug_assertions, feature = "feat-tracing")
        ),
        tracing::instrument(level = "TRACE", ret)
    )]
    /// Returns the ideal length of target splice ken based on the current rate
    /// limit.
    pub(crate) fn ideal_len(&self, pipe_size: NonZeroUsize) -> Option<NonZeroUsize> {
        let Some(limit) = self.limit.current() else {
            // Rate limiter is disabled, return the pipe size.
            return None;
        };

        let ideal_len = min(
            (limit.get() as f64 * TOKIO_TIMER_MIN_DUR.as_secs_f64()).ceil() as usize * 2,
            pipe_size.get(),
        );

        #[allow(unsafe_code)]
        // `ideal_len` is guaranteed to be non-zero.
        Some(unsafe { NonZeroUsize::new_unchecked(ideal_len) })
    }

    #[cfg_attr(
        any(
            feature = "feat-tracing-trace",
            all(debug_assertions, feature = "feat-tracing")
        ),
        tracing::instrument(level = "TRACE", ret)
    )]
    /// Check if the rate limiter allows the request to proceed, or sleep.
    pub(crate) fn check(&mut self, has_read: NonZeroUsize) -> RateLimitResult {
        if !ENABLED {
            // Rate limiter is disabled, always accept.
            return RateLimitResult::Accepted;
        }

        let Some(limit) = self.limit.current() else {
            // Rate limiter is disabled, always accept.
            self.tokens = None;
            self.last_updated = None;

            return RateLimitResult::Accepted;
        };

        let now = Instant::now();

        let Some(ref mut last_updated) = self.last_updated else {
            // Initialize the last updated time.
            self.last_updated = Some(now);

            return RateLimitResult::Accepted;
        };

        let current_tokens = if let Some(ref mut tokens) = self.tokens {
            tokens
        } else {
            // Initialize the tokens and last updated time.
            self.tokens = Some(limit.get() as f64 * TOKIO_TIMER_MIN_DUR.as_secs_f64());
            self.tokens.as_mut().unwrap()
        };

        // Refill the tokens bucket
        Self::refill(current_tokens, now, last_updated, limit);

        // Try to acquire the tokens.
        {
            *current_tokens -= has_read.get() as f64;

            if current_tokens.is_sign_negative() {
                let insufficient_tokens = current_tokens.abs();

                return RateLimitResult::Throttled {
                    now,
                    dur: Duration::from_secs_f64(
                        (insufficient_tokens / limit.get() as f64).floor(),
                    )
                    .max(TOKIO_TIMER_MIN_DUR), // or Tokio may sleep forever
                };
            }
        }

        RateLimitResult::Accepted
    }

    #[inline]
    /// Refill the token bucket with the elapsed time since the last update.
    fn refill(tokens: &mut f64, now: Instant, last_updated: &mut Instant, limit: NonZeroU64) {
        let Some(elapsed) = now.checked_duration_since(*last_updated) else {
            // The last update is in the future, force update.
            *last_updated = now;

            return;
        };

        *last_updated = now;

        let new_tokens = *tokens + (limit.get() as f64 * elapsed.as_secs_f64());
        let max_new_tokens = limit.get() as f64 * TOKIO_TIMER_MIN_DUR.as_secs_f64();

        // Enforce the tokens to be in the range of [0, limit * 0.01s].
        if new_tokens.is_normal() {
            // After a long time with few tokens acquired, we will have a large amount of
            // tokens accumulated, this is not what we want.

            *tokens = if max_new_tokens <= new_tokens {
                max_new_tokens
            } else {
                new_tokens
            };
        } else {
            // Rare.
            *tokens = max_new_tokens;
        }
    }
}
