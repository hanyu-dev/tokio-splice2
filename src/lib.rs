//! tokio-splice2

#![cfg_attr(feature = "feat-nightly", feature(cold_path))]

mod pipe;
mod splice;

use std::io;
use std::pin::Pin;

pub use splice::{AsyncReadFd, AsyncStreamFd, AsyncWriteFd, SpliceIoCtx};

/// Copy data from `r` to `w` using `splice(2)`.
///
/// This is a convenience function that uses [`SpliceIoCtx::copy`].
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
/// This is a convenience function that uses
/// [`SpliceIoCtx::copy_bidirectional`].
pub async fn copy_bidirectional<R, W>(a: &mut R, b: &mut W) -> io::Result<(usize, usize)>
where
    R: splice::AsyncStreamFd + Unpin,
    W: splice::AsyncStreamFd + Unpin,
{
    SpliceIoCtx::prepare(None, None, None)?
        .copy_bidirectional(Pin::new(a), Pin::new(b))
        .await
}
