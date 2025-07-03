//! Traffic transmitted result.

use std::io;

use crate::SpliceIoCtx;

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
    pub(crate) fn new<A, B>(
        ctx_a_to_b: &SpliceIoCtx<A, B>,
        ctx_b_to_a: Option<&SpliceIoCtx<B, A>>,
        error: Option<io::Error>,
    ) -> io::Result<Self> {
        match error {
            Some(err) => match err.kind() {
                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset => Ok(Self {
                    tx: ctx_a_to_b.has_read().max(ctx_a_to_b.has_written()),
                    rx: ctx_b_to_a.map_or(0, |ctx| ctx.has_read().max(ctx.has_written())),
                    error: None,
                }),
                _ => Err(err),
            },
            None => Ok(Self {
                tx: ctx_a_to_b.has_written(),
                rx: ctx_b_to_a.map_or(0, |ctx| ctx.has_written()),
                error: None,
            }),
        }
    }

    #[inline]
    /// Creates a new [`TrafficResult`] from an `io::Result<T>`.
    pub fn new_from_io_result<A, B, T>(
        ctx_a_to_b: &SpliceIoCtx<A, B>,
        ctx_b_to_a: Option<&SpliceIoCtx<B, A>>,
        result: io::Result<T>,
    ) -> io::Result<Self> {
        match result {
            Ok(_) => Self::new(ctx_a_to_b, ctx_b_to_a, None),
            Err(e) => Self::new(ctx_a_to_b, ctx_b_to_a, Some(e)),
        }
    }

    #[inline]
    /// Merges two `TrafficResult` instances.
    pub fn merge(self, other: Self) -> Self {
        Self {
            tx: self.tx + other.tx,
            rx: self.rx + other.rx,
            error: self.error.or(other.error),
        }
    }

    #[inline]
    /// Returns the total number of bytes transmitted in both directions.
    pub const fn sum(&self) -> usize {
        self.tx.saturating_add(self.rx)
    }
}
