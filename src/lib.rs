#![doc = include_str!("../README.md")]
#![cfg_attr(feature = "feat-nightly", feature(cold_path))]
#![cfg_attr(debug_assertions, allow(clippy::unreachable))]

pub mod context;
pub mod io;
pub mod pipe;
#[cfg(feature = "feat-rate-limit")]
pub mod rate;
pub mod traffic;
pub mod utils;

#[cfg(not(feature = "feat-rate-limit"))]
pub mod rate {
    //! TCP rate limiter implementation.
    //!
    //! This module provides a no-op implementation of the rate limiter if the
    //! `feat-rate-limit` feature is not enabled.

    #[allow(unused)]
    pub(crate) const RATE_LIMITER_ENABLED: bool = true;
    #[allow(unused)]
    pub(crate) const RATE_LIMITER_DISABLED: bool = false;
}

pub use context::SpliceIoCtx;
pub use io::{AsyncReadFd, AsyncWriteFd, IsFile, IsNotFile, SpliceBidiIo, SpliceIo};
#[cfg(feature = "feat-rate-limit")]
pub use rate::RateLimit;

#[inline]
/// Copies data from `r` to `w` using `splice(2)`.
///
/// See [`SpliceIoCtx::prepare`] and [`SpliceIo::execute`] for more details; see
/// the [crate-level documentation](crate) for known limitations.
///
/// ## Errors
///
/// * Create pipe failed.
pub async fn copy<R, W>(r: &mut R, w: &mut W) -> std::io::Result<traffic::TrafficResult>
where
    R: io::AsyncReadFd + IsNotFile + Unpin,
    W: io::AsyncWriteFd + IsNotFile + Unpin,
{
    Ok(context::SpliceIoCtx::prepare()?
        .into_io()
        .execute(r, w)
        .await)
}

#[inline]
/// Copies data from file `r` to `w` using `splice(2)`.
///
/// See [`SpliceIoCtx::prepare_reading_file`] for more details; see the
/// [crate-level documentation](crate) for known limitations.
///
/// ## Errors
///
/// * Create pipe failed.
/// * Invalid file length or offset.
pub async fn sendfile<R, W>(
    r: &mut R,
    w: &mut W,
    f_len: u64,
    f_offset_start: Option<u64>,
    f_offset_end: Option<u64>,
) -> std::io::Result<traffic::TrafficResult>
where
    R: io::AsyncReadFd + IsFile + Unpin,
    W: io::AsyncWriteFd + IsNotFile + Unpin,
{
    Ok(
        context::SpliceIoCtx::prepare_reading_file(f_len, f_offset_start, f_offset_end)?
            .into_io()
            .execute(r, w)
            .await,
    )
}

#[inline]
/// Copies data in both directions between `sl` and `sr`.
///
/// This function returns a future that will read from both streams, writing any
/// data read to the opposing stream. This happens in both directions
/// concurrently.
///
/// See [`SpliceIoCtx::prepare`] and [`SpliceBidiIo::execute`] for more details;
/// see the [crate-level documentation](crate) for known limitations.
///
/// ## Errors
///
/// * Create pipe failed.
pub async fn copy_bidirectional<A, B>(
    sl: &mut A,
    sr: &mut B,
) -> std::io::Result<traffic::TrafficResult>
where
    A: io::AsyncReadFd + io::AsyncWriteFd + IsNotFile + Unpin,
    B: io::AsyncReadFd + io::AsyncWriteFd + IsNotFile + Unpin,
{
    Ok(io::SpliceBidiIo {
        io_sl2sr: context::SpliceIoCtx::prepare()?.into_io(),
        io_sr2sl: context::SpliceIoCtx::prepare()?.into_io(),
    }
    .execute(sl, sr)
    .await)
}

// === Tracing macros for logging ===

#[allow(unused)]
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

#[allow(unused)]
macro_rules! info {
    ($($tt:tt)*) => {{
        #[cfg(feature = "feat-tracing")]
        tracing::info!($($tt)*);
    }};
}

#[allow(unused)]
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
macro_rules! enter_tracing_span {
    ($($tt:tt)*) => {
        #[cfg(any(
            feature = "feat-tracing-trace",
            all(debug_assertions, feature = "feat-tracing")
        ))]
        let _span = tracing::span!(
            tracing::Level::TRACE,
            $($tt)*
        )
        .entered();
    };
}

#[allow(unused)]
pub(crate) use {debug, enter_tracing_span, error, info, trace, warning};
