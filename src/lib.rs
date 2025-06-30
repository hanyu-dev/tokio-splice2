//! tokio-splice2

#![cfg_attr(feature = "feat-nightly", feature(cold_path))]
#![cfg_attr(debug_assertions, allow(clippy::unreachable))]

mod pipe;
mod splice;

use std::io;
use std::pin::Pin;

pub use splice::{AsyncReadFd, AsyncWriteFd, ReadFd, SpliceIoCtx, WriteFd};

/// Copy data from `r` to `w` using `splice(2)`.
///
/// This is a convenience function that uses [`SpliceIoCtx::copy`] with default
/// ctx. See [`SpliceIoCtx::copy`] for more details.
pub async fn copy<R, W>(r: &mut R, w: &mut W) -> io::Result<usize>
where
    R: splice::AsyncReadFd + Unpin,
    W: splice::AsyncWriteFd + Unpin,
{
    SpliceIoCtx::prepare(None, None, None)?
        .copy(Pin::new(r), Pin::new(w))
        .await
}

/// Copies data in both directions between `a` and `b`.
///
/// This function returns a future that will read from both streams, writing any
/// data read to the opposing stream. This happens in both directions
/// concurrently.
///
/// This is a shortcut of [`SpliceIoCtx::copy_bidirectional`].
pub async fn copy_bidirectional<A, B>(a: &mut A, b: &mut B) -> io::Result<(usize, usize)>
where
    A: splice::AsyncReadFd + splice::AsyncWriteFd + Unpin,
    B: splice::AsyncReadFd + splice::AsyncWriteFd + Unpin,
{
    SpliceIoCtx::copy_bidirectional(Pin::new(a), Pin::new(b)).await
}

/// Copy data from `r` to `w` using `splice(2)`, but blocking.
///
/// This is a convenience function that uses [`SpliceIoCtx::blocking_copy`] with
/// default ctx. See [`SpliceIoCtx::blocking_copy`] for more details.
pub fn blocking_copy<R, W>(r: &mut R, w: &mut W) -> io::Result<usize>
where
    R: splice::ReadFd,
    W: splice::WriteFd,
{
    SpliceIoCtx::prepare(None, None, None)?.blocking_copy(r, w)
}
