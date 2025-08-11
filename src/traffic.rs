//! Traffic transmitted result.

use std::io;

#[derive(Debug)]
/// Traffic transmitted throughout the `splice(2)` operation, regardless of any
/// errors.
pub struct TrafficResult {
    /// The number of bytes that have been transferred from a to b
    pub tx: usize,

    /// The number of bytes that have been transferred from b to a.
    pub rx: usize,

    /// The error that occurred during the `splice(2)` operation, if any.
    pub error: Option<io::Error>,
}

impl TrafficResult {
    #[must_use]
    #[inline]
    /// Merges two `TrafficResult` instances.
    pub fn merge(self, other: Self) -> Self {
        Self {
            tx: self.tx + other.tx,
            rx: self.rx + other.rx,
            error: self.error.or(other.error),
        }
    }

    #[must_use]
    #[inline]
    /// Returns the total number of bytes transmitted in both directions.
    pub const fn sum(&self) -> usize {
        self.tx.saturating_add(self.rx)
    }

    #[inline]
    /// Turns the `TrafficResult` into an `io::Result<usize>`.
    ///
    /// ## Errors
    ///
    /// Extracts the error from the `TrafficResult` if it exists.
    pub fn into_result(self) -> io::Result<Self> {
        if let Some(err) = self.error {
            Err(err)
        } else {
            Ok(TrafficResult {
                error: None,
                ..self
            })
        }
    }
}
