//! `tokio-splice2` - `splice(2)` syscall helper in async Rust.
//!
//! See [`splice(2)`] for more details.
//!
//! ## Reminders
//!
//! - When splicing data from a file to a pipe and then splicing from the pipe
//!   to a socket for transmission, the data is referenced from the page cache
//!   corresponding to the file. If the original file is modified while the
//!   splice operation is in progress (i.e., the data is still in the kernel
//!   buffer and has not been fully sent to the network), there may be a
//!   situation where the transmitted data is the old data (before
//!   modification). Because there is no clear mechanism to know when the data
//!   has truly "left" the kernel and been sent to the network, thus safely
//!   allowing the file to be modified. Linus Torvalds once commented that this
//!   is the "key point" of splice design, which shares references to data pages
//!   and behaves similarly to `mmap()`. This is a complex issue concerning data
//!   consistency and concurrent access.
//!
//!   See [lwn.net/Articles/923237] and [rust#116451].
//!
//!   This crate requires passing `&mut R` to prevent modification elsewhere
//!   before the `Future` of `splice(2)` I/O completes. However, this is just
//!   best-effort guarantee.
//!
//! - In certain cases, such as transferring small chunks of data, frequently
//!   calling splice, or when the underlying driver/hardware does not support
//!   efficient zero-copy, the performance improvement may not meet
//!   expectations. It could even be lower than an optimized read/write loop due
//!   to additional system call overhead. The choice of pipe buffer size may
//!   also affect performance.
//!
//!   Performance testing using `iperf3` demonstrated a ~**15%** decrease in
//!   maximum throughput when compared to establishing direct connections.
//!
//! - A successful [`splice(2)`] call returns the number of bytes transferred,
//!   but this ONLY indicates that the data has entered the kernel buffer of the
//!   destination file descriptor (such as the send buffer of a socket). It does
//!   not mean the data has actually left the local network interface or been
//!   received by the peer.
//!
//!   We call `flush` / `poll_flush` after bytes data is spliced from pipe to
//!   the target fd to ensure that it has been flushed to the destination.
//!   However, poor implementation of `std::io::Write` / `tokio::io::AsyncWrite`
//!   may break this guarantee, as they may not flush the data immediately.
//!
//! [`splice(2)`]: https://man7.org/linux/man-pages/man2/splice.2.html
//! [lwn.net/Articles/923237]: https://lwn.net/Articles/923237/
//! [rust#116451]: https://github.com/rust-lang/rust/issues/116451

#![cfg_attr(feature = "feat-nightly", feature(cold_path))]
#![cfg_attr(debug_assertions, allow(clippy::unreachable))]

pub mod pipe;
mod splice;
pub mod traffic;

use std::io;
use std::pin::Pin;

pub use splice::{AsyncReadFd, AsyncWriteFd, IsFile, IsNotFile, ReadFd, SpliceIoCtx, WriteFd};

#[inline]
/// Copy data from `r` to `w` using `splice(2)`.
///
/// This is a convenience function that uses [`SpliceIoCtx::copy`] with default
/// ctx. See [`SpliceIoCtx::copy`] for more details.
///
/// Notice: see the [module-level documentation](crate) for known limitations.
pub async fn copy<R, W>(r: &mut R, w: &mut W) -> io::Result<traffic::TrafficResult>
where
    R: splice::AsyncReadFd + IsNotFile + Unpin,
    W: splice::AsyncWriteFd + IsNotFile + Unpin,
{
    SpliceIoCtx::prepare()?.copy(Pin::new(r), Pin::new(w)).await
}

#[inline]
/// Copy data from file `r` to `w` using `splice(2)`.
///
/// Unlike [`copy`], this function is specifically designed for copying from a
/// file.
///
/// Notice: see the [module-level documentation](crate) for known limitations.
pub async fn copy_from_file<R, W>(
    r: &mut R,
    w: &mut W,
    f_len: u64,
    f_offset_start: Option<u64>,
    f_offset_end: Option<u64>,
) -> io::Result<traffic::TrafficResult>
where
    R: splice::AsyncReadFd + IsFile + Unpin,
    W: splice::AsyncWriteFd + IsNotFile + Unpin,
{
    SpliceIoCtx::prepare_reading_file(f_len, f_offset_start, f_offset_end)?
        .copy(Pin::new(r), Pin::new(w))
        .await
}

#[inline]
/// Blocking version of [`copy`].
pub fn blocking_copy<R, W>(r: &mut R, w: &mut W) -> io::Result<traffic::TrafficResult>
where
    R: splice::ReadFd + IsNotFile,
    W: splice::WriteFd + IsNotFile,
{
    SpliceIoCtx::prepare()?.blocking_copy(r, w)
}

#[inline]
/// Blocking version of [`copy_from_file`].
pub fn blocking_copy_from_file<R, W>(
    r: &mut R,
    w: &mut W,
    f_len: u64,
    f_offset_start: Option<u64>,
    f_offset_end: Option<u64>,
) -> io::Result<traffic::TrafficResult>
where
    R: splice::ReadFd + IsFile,
    W: splice::WriteFd + IsNotFile,
{
    SpliceIoCtx::prepare_reading_file(f_len, f_offset_start, f_offset_end)?.blocking_copy(r, w)
}

#[inline]
/// Copies data in both directions between `a` and `b`.
///
/// This function returns a future that will read from both streams, writing any
/// data read to the opposing stream. This happens in both directions
/// concurrently.
///
/// Notice: see the [module-level documentation](crate) for known limitations.
pub async fn copy_bidirectional<A, B>(a: &mut A, b: &mut B) -> io::Result<traffic::TrafficResult>
where
    A: splice::AsyncReadFd + splice::AsyncWriteFd + IsNotFile + Unpin,
    B: splice::AsyncReadFd + splice::AsyncWriteFd + IsNotFile + Unpin,
{
    SpliceIoCtx::copy_bidirectional(
        SpliceIoCtx::prepare()?,
        SpliceIoCtx::prepare()?,
        Pin::new(a),
        Pin::new(b),
    )
    .await
}

macro_rules! trace {
    ($($tt:tt)*) => {{
        #[cfg(any(feature = "feat-tracing-trace", all(debug_assertions, feature = "feat-tracing")))]
        tracing::trace!($($tt)*);
    }};
}

#[allow(unused)]
macro_rules! debug {
    ($($tt:tt)*) => {{
        #[cfg(feature = "feat-tracing")]
        tracing::debug!($($tt)*);
    }};
}

macro_rules! info {
    ($($tt:tt)*) => {{
        #[cfg(feature = "feat-tracing")]
        tracing::info!($($tt)*);
    }};
}

// Avoid name conflicts with `warn` in the standard library.
macro_rules! warning {
    ($($tt:tt)*) => {{
        #[cfg(feature = "feat-tracing")]
        tracing::warn!($($tt)*);
    }};
}

#[allow(unused)]
macro_rules! error {
    ($($tt:tt)*) => {{
        #[cfg(feature = "feat-tracing")]
        tracing::error!($($tt)*);
    }};
}

#[allow(unused)]
pub(crate) use {debug, error, info, trace, warning};
