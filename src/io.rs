//! `splice(2)` I/O implementation.

use core::ops;
use std::fs::File;
use std::future::poll_fn;
use std::io;
use std::marker::PhantomData;
#[cfg(not(feature = "feat-rate-limit"))]
use std::marker::PhantomPinned;
use std::net::TcpStream;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::pin::{pin, Pin};
use std::task::{ready, Context, Poll};

use crossbeam_utils::CachePadded;
use tokio::fs::File as AsyncFile;
use tokio::io::{AsyncRead, AsyncWrite, Interest};
use tokio::net::{TcpStream as AsyncTcpStream, UnixStream as AsyncUnixStream};
#[cfg(feature = "feat-rate-limit")]
use tokio::time::Sleep;

use crate::context::SpliceIoCtx;
use crate::rate::RATE_LIMITER_DISABLED;
#[cfg(feature = "feat-rate-limit")]
use crate::rate::{RateLimit, RateLimitResult, RateLimiter, RATE_LIMITER_ENABLED};
use crate::traffic::TrafficResult;
use crate::utils::Drained;

#[pin_project::pin_project]
#[derive(Debug)]
/// Zero-copy unidirectional I/O with `splice(2)`.
///
/// For bidirectional I/O version, see [`SpliceBidiIo`].
///
/// Notice: see the [module-level documentation](crate) for known limitations.
pub struct SpliceIo<R, W, const RATE_LIMITER_IS_ENABLED: bool = RATE_LIMITER_DISABLED> {
    /// Context for the splice I/O operation.
    ///
    /// See [`SpliceIoCtx`] for more details.
    ctx: CachePadded<SpliceIoCtx<R, W>>,

    r: PhantomData<R>,
    w: PhantomData<W>,

    #[cfg(feature = "feat-rate-limit")]
    /// To limit the transfer speed.
    rate_limiter: RateLimiter<RATE_LIMITER_IS_ENABLED>,

    #[pin]
    state: TransferState,
}

impl<R, W, const RATE_LIMITER_IS_ENABLED: bool> ops::Deref
    for SpliceIo<R, W, RATE_LIMITER_IS_ENABLED>
{
    type Target = SpliceIoCtx<R, W>;

    fn deref(&self) -> &Self::Target {
        &self.ctx
    }
}

impl<R, W> SpliceIo<R, W, RATE_LIMITER_DISABLED> {
    /// Create a new `SpliceIo` instance with ctx and pinned `R` / `W`.
    pub fn new(ctx: SpliceIoCtx<R, W>) -> Self {
        SpliceIo {
            ctx: CachePadded::new(ctx),
            r: PhantomData,
            w: PhantomData,
            #[cfg(feature = "feat-rate-limit")]
            rate_limiter: RateLimiter::empty(),
            state: TransferState::Draining,
        }
    }

    #[cfg(feature = "feat-rate-limit")]
    /// Apply rate limitation during the splice I/O.
    ///
    /// See [`RateLimit`] for more details.
    pub fn with_rate_limit(self, limit: RateLimit) -> SpliceIo<R, W, RATE_LIMITER_ENABLED> {
        SpliceIo {
            ctx: self.ctx,
            r: self.r,
            w: self.w,
            rate_limiter: RateLimiter::new(limit),
            state: self.state,
        }
    }
}

#[derive(Debug)]
#[pin_project::pin_project(project = TransferStateProj)]
enum TransferState {
    /// Draining data from `R` to pipe.
    Draining,

    #[cfg_attr(not(feature = "feat-rate-limit"), allow(dead_code))]
    /// Rate limiter is throttling the I/O operations.
    Throttled {
        #[cfg(feature = "feat-rate-limit")]
        #[pin]
        sleep: Sleep,

        #[cfg(not(feature = "feat-rate-limit"))]
        #[doc(hidden)]
        #[pin]
        // Make `pin-project` happy.
        _pinned: PhantomPinned,
    },

    /// Pumping data from pipe to `W`.
    Pumping,

    /// Flushing buffered data of `W`.
    Flushing,

    /// Transfer is finished, `W` is shutting down.
    Terminating,

    /// An error occurred during the transfer.
    Faulted { error: Option<io::Error> },

    /// Transfer is finished.
    Finished,
}

impl<R, W, const RATE_LIMITER_IS_ENABLED: bool> SpliceIo<R, W, RATE_LIMITER_IS_ENABLED>
where
    R: AsyncReadFd,
    W: AsyncWriteFd,
{
    /// Performs zero-copy data transfer from reader `R` to writer `W` using the
    /// splice syscall.
    ///
    /// This is a convenient `async fn` version of
    /// [`SpliceIo::poll_execute`].
    pub async fn execute(self, r: &mut R, w: &mut W) -> TrafficResult
    where
        R: Unpin,
        W: Unpin,
    {
        let mut this = pin!(self);
        let mut r = Pin::new(r);
        let mut w = Pin::new(w);

        let error = poll_fn(|cx| this.as_mut().poll_execute(cx, r.as_mut(), w.as_mut()))
            .await
            .err();

        this.ctx.traffic_client_tx(error)
    }

    #[cfg_attr(
        any(
            feature = "feat-tracing-trace",
            all(debug_assertions, feature = "feat-tracing")
        ),
        tracing::instrument(level = "TRACE", skip(self, cx, r, w), ret)
    )]
    /// Performs zero-copy data transfer from reader `R` to writer `W` using the
    /// splice syscall.
    ///
    /// This is the `poll`-based asynchronous version.
    ///
    /// # Notes
    ///
    /// This is an advanced API that should only be used if you fully understand
    /// its behavior. When using this API:
    ///
    /// - The [`SpliceIo`] instance MUST NOT be reused after completion.
    /// - The caller MAY manually extracts [`TrafficResult`] from the context.
    pub fn poll_execute(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut r: Pin<&mut R>,
        mut w: Pin<&mut W>,
    ) -> Poll<io::Result<()>> {
        macro_rules! ready_or_cleanup {
            ($e:expr, $state:expr) => {
                match $e {
                    Poll::Ready(Ok(t)) => t,
                    Poll::Ready(Err(e)) => {
                        $state.set(TransferState::Faulted { error: Some(e) });
                        continue;
                    }
                    Poll::Pending => {
                        break Poll::Pending;
                    }
                }
            };
        }

        loop {
            crate::enter_tracing_span!(
                "loop",
                ctx = ?self.ctx,
                state = ?self.state,
            );

            let mut this = self.as_mut().project();

            match this.state.as_mut().project() {
                TransferStateProj::Draining => {
                    #[cfg(feature = "feat-rate-limit")]
                    let ideal_len = this.rate_limiter.ideal_len(this.ctx.pipe_size());

                    #[cfg(not(feature = "feat-rate-limit"))]
                    let ideal_len = None;

                    match ready_or_cleanup!(
                        this.ctx.poll_splice_drain(cx, r.as_mut(), ideal_len),
                        this.state.as_mut()
                    ) {
                        Drained::Some(_drained) => {
                            #[cfg(feature = "feat-rate-limit")]
                            {
                                match this.rate_limiter.check(_drained) {
                                    RateLimitResult::Accepted => {}
                                    RateLimitResult::Throttled { now, dur } => {
                                        this.state.as_mut().set(TransferState::Throttled {
                                            sleep: tokio::time::sleep_until(now + dur),
                                        });
                                        continue;
                                    }
                                }
                            }
                        }
                        Drained::Done => {}
                    }

                    this.state.set(TransferState::Pumping);
                }
                #[cfg(feature = "feat-rate-limit")]
                TransferStateProj::Throttled { sleep } => {
                    use std::future::Future;

                    ready!(sleep.poll(cx));

                    // After throttled, we shall continue to pump data from pipe to `W`.
                    this.state.set(TransferState::Pumping);
                }
                #[cfg(not(feature = "feat-rate-limit"))]
                TransferStateProj::Throttled { _pinned } => {
                    // Actually, this branch should never be reached.
                    this.state.set(TransferState::Pumping);
                }
                TransferStateProj::Pumping => {
                    ready_or_cleanup!(
                        this.ctx.poll_splice_pump(cx, w.as_mut()),
                        this.state.as_mut()
                    );

                    if this.ctx.finished() {
                        // All done, flush and shutdown `W`.
                        this.state.set(TransferState::Terminating);
                    } else {
                        // Flush `W` after pumping data.
                        this.state.set(TransferState::Flushing);
                    }
                }
                TransferStateProj::Flushing => {
                    ready_or_cleanup!(w.as_mut().poll_flush(cx), this.state.as_mut());

                    this.state.set(TransferState::Draining);
                }
                TransferStateProj::Terminating => {
                    ready_or_cleanup!(w.as_mut().poll_shutdown(cx), this.state.as_mut());

                    this.state.set(TransferState::Finished);
                }
                TransferStateProj::Faulted { error } => {
                    if error.is_some() {
                        // Best effort to shutdown the writer.
                        ready!(w.as_mut().poll_shutdown(cx))?;
                    } else {
                        #[cfg(feature = "feat-nightly")]
                        std::hint::cold_path();
                    }

                    let Some(error) = error.take() else {
                        #[cfg(feature = "feat-nightly")]
                        std::hint::cold_path();

                        break Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::Other,
                            "`poll_execute()` called after error returned",
                        )));
                    };

                    break Poll::Ready(Err(error));
                }
                TransferStateProj::Finished => {
                    break Poll::Ready(Ok(()));
                }
            }
        }
    }
}

#[pin_project::pin_project]
#[derive(Debug)]
/// Bidirectional splice I/O, combining two `SpliceIo` instances.
pub struct SpliceBidiIo<
    SL,
    SR,
    const SL_RATE_LIMITER_IS_ENABLED: bool,
    const SR_RATE_LIMITER_IS_ENABLED: bool,
> {
    #[pin]
    /// Splice I/O instance, from `SL` to `SR`.
    pub io_sl2sr: SpliceIo<SL, SR, SL_RATE_LIMITER_IS_ENABLED>,

    #[pin]
    /// Splice I/O instance, from `SR` to `SL`.
    pub io_sr2sl: SpliceIo<SR, SL, SR_RATE_LIMITER_IS_ENABLED>,
}

impl<SL, SR, const SL_RATE_LIMITER_IS_ENABLED: bool, const SR_RATE_LIMITER_IS_ENABLED: bool>
    SpliceBidiIo<SL, SR, SL_RATE_LIMITER_IS_ENABLED, SR_RATE_LIMITER_IS_ENABLED>
where
    SL: AsyncReadFd + AsyncWriteFd + IsNotFile,
    SR: AsyncReadFd + AsyncWriteFd + IsNotFile,
{
    /// Performs zero-copy data transfer between `SL` and `SR` using the
    /// splice syscall.
    ///
    /// This is a convenient `async fn` version of
    /// [`SpliceBidiIo::poll_execute`].
    pub async fn execute(self, sl: &mut SL, sr: &mut SR) -> TrafficResult
    where
        SL: Unpin,
        SR: Unpin,
    {
        let mut this = pin!(self);
        let mut sl = Pin::new(sl);
        let mut sr = Pin::new(sr);

        let error = poll_fn(|cx| this.as_mut().poll_execute(cx, sl.as_mut(), sr.as_mut()))
            .await
            .err();

        // After copy done, we can return the traffic result.
        this.io_sl2sr
            .ctx
            .traffic_client_tx(error)
            .merge(this.io_sr2sl.ctx.traffic_client_rx(None))
    }

    #[cfg_attr(
        any(
            feature = "feat-tracing-trace",
            all(debug_assertions, feature = "feat-tracing")
        ),
        tracing::instrument(
            level = "TRACE",
            name = "SpliceBidiIo::poll_execute",
            skip(self, cx, sl, sr),
            ret
        )
    )]
    /// Performs zero-copy data transfer between `SL` and `SR` using the
    /// splice syscall.
    ///
    /// This is the `poll`-based asynchronous version.
    ///
    /// # Notes
    ///
    /// This is an advanced API that should only be used if you fully understand
    /// its behavior. When using this API:
    ///
    /// - The [`SpliceBidiIo`] instance MUST NOT be reused after completion.
    /// - The caller MAY manually extracts [`TrafficResult`] from the context.
    pub fn poll_execute(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut sl: Pin<&mut SL>,
        mut sr: Pin<&mut SR>,
    ) -> Poll<io::Result<()>> {
        let mut this = self.project();

        let io_sl2sr_ret = this
            .io_sl2sr
            .as_mut()
            .poll_execute(cx, sl.as_mut(), sr.as_mut());
        let io_sr2sl_ret = this
            .io_sr2sl
            .as_mut()
            .poll_execute(cx, sr.as_mut(), sl.as_mut());

        match (io_sl2sr_ret, io_sr2sl_ret) {
            (Poll::Pending, _) | (_, Poll::Pending) => Poll::Pending,
            (Poll::Ready(Ok(())), Poll::Ready(Ok(()))) => Poll::Ready(Ok(())),
            (Poll::Ready(Err(e)), _) | (_, Poll::Ready(Err(e))) => Poll::Ready(Err(e)),
        }
    }
}

// === traits ===

/// Marker trait: indicate a file.
///
/// Since the compiler complains *conflicting implementations* when we try to
/// implement `IsFile` for `T: ops::Deref<U>` when U: `IsFile`, you have to
/// implement this marker trait for your wrapper type over a file.
pub trait IsFile {}

impl<T> IsFile for &mut T where T: IsFile {}
impl<T> IsFile for Pin<&mut T> where T: IsFile {}

/// Marker trait: indicate not a file.
///
/// We have to introduce this because Rust does not allow the syntax `!IsFile`
/// (at least only limited to some builtin marker traits like `Send`),
pub trait IsNotFile {}

impl<T> IsNotFile for &mut T where T: IsNotFile {}
impl<T> IsNotFile for Pin<&mut T> where T: IsNotFile {}

/// Marker trait: indicates an async-readable file descriptor.
///
/// This trait extends both `AsyncRead` and `AsFd`, providing the necessary
/// methods for async reading operations with splice.
pub trait AsyncReadFd: AsyncRead + AsFd {
    #[doc(hidden)]
    fn poll_read_ready(&self, _cx: &mut Context<'_>) -> Poll<io::Result<()>>;

    #[doc(hidden)]
    fn try_io_read<R>(&self, f: impl FnOnce() -> io::Result<R>) -> io::Result<R>;
}

impl<T: AsyncReadFd + Unpin> AsyncReadFd for &mut T {
    fn poll_read_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        (**self).poll_read_ready(cx)
    }

    fn try_io_read<R>(&self, f: impl FnOnce() -> io::Result<R>) -> io::Result<R> {
        (**self).try_io_read(f)
    }
}

/// Marker trait: indicates an async-writable file descriptor.
///
/// This trait extends both `AsyncWrite` and `AsFd`, providing the necessary
/// methods for async writing operations with splice.
pub trait AsyncWriteFd: AsyncWrite + AsFd {
    #[doc(hidden)]
    fn poll_write_ready(&self, _cx: &mut Context<'_>) -> Poll<io::Result<()>>;

    #[doc(hidden)]
    fn try_io_write<R>(&self, f: impl FnOnce() -> io::Result<R>) -> io::Result<R>;
}

impl<T: AsyncWriteFd + Unpin> AsyncWriteFd for &mut T {
    fn poll_write_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        (**self).poll_write_ready(cx)
    }

    fn try_io_write<R>(&self, f: impl FnOnce() -> io::Result<R>) -> io::Result<R> {
        (**self).try_io_write(f)
    }
}

macro_rules! impl_async_fd {
    ($($ty:ty),+) => {
        $(
            impl AsyncReadFd for $ty {
                fn poll_read_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                    self.poll_read_ready(cx)
                }

                fn try_io_read<R>(&self, f: impl FnOnce() -> io::Result<R>) -> io::Result<R> {
                    self.try_io(Interest::READABLE, f)
                }
            }

            impl AsyncWriteFd for $ty {
                fn poll_write_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                    self.poll_write_ready(cx)
                }

                fn try_io_write<R>(&self, f: impl FnOnce() -> io::Result<R>) -> io::Result<R> {
                    self.try_io(Interest::WRITABLE, f)
                }
            }

            impl IsNotFile for $ty {}
        )+
    };
    (FILE: $($ty:ty),+) => {
        $(
            impl AsyncReadFd for $ty {
                fn poll_read_ready(&self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                    Poll::Ready(Ok(()))
                }

                fn try_io_read<R>(&self, f: impl FnOnce() -> io::Result<R>) -> io::Result<R> {
                    f()
                }
            }

            impl AsyncWriteFd for $ty {
                fn poll_write_ready(&self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                    Poll::Ready(Ok(()))
                }

                fn try_io_write<R>(&self, f: impl FnOnce() -> io::Result<R>) -> io::Result<R> {
                    f()
                }
            }

            impl IsFile for $ty {}
        )+
    };
}

impl_async_fd!(AsyncTcpStream, AsyncUnixStream);
impl_async_fd!(FILE: AsyncFile);

/// Trait for readable file descriptors.
pub trait ReadFd: io::Read + AsFd {}

impl<T: ReadFd> ReadFd for &mut T {}

/// Trait for writable file descriptors.
pub trait WriteFd: io::Write + AsFd {}

impl<T: WriteFd> WriteFd for &mut T {}

macro_rules! impl_fd {
    ($($ty:ty),+) => {
        $(
            impl ReadFd for $ty {}
            impl WriteFd for $ty {}
            impl IsNotFile for $ty {}
        )+
    };
    (FILE: $($ty:ty),+) => {
        $(
            impl ReadFd for $ty {}
            impl WriteFd for $ty {}
            impl IsFile for $ty {}
        )+
    };
}

impl_fd!(TcpStream, UnixStream);
impl_fd!(FILE: File);
