//! `splice(2)` IO implementation.

use std::fs::File;
use std::future::poll_fn;
use std::marker::PhantomData;
use std::net::TcpStream;
use std::num::NonZeroUsize;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::{fmt, io};

use rustix::pipe::{splice, SpliceFlags};
use tokio::fs::File as AsyncFile;
use tokio::io::{AsyncRead, AsyncWrite, Interest};
use tokio::net::{TcpStream as AsyncTcpStream, UnixStream as AsyncUnixStream};

use crate::pipe::Pipe;
use crate::traffic::TrafficResult;

#[derive(Debug)]
pub(crate) enum Offset {
    None,
    /// Read offset set.
    In(Option<u64>),
    /// Write offset set.
    Out(Option<u64>),
}

impl Offset {
    #[inline]
    fn off_in(&mut self) -> Option<&mut u64> {
        match self {
            Offset::In(off) => off.as_mut(),
            _ => None,
        }
    }

    #[inline]
    fn off_out(&mut self) -> Option<&mut u64> {
        match self {
            Offset::Out(off) => off.as_mut(),
            _ => None,
        }
    }

    fn calc_size_to_splice(
        f_len: u64,
        f_offset_start: Option<u64>,
        f_offset_end: Option<u64>,
    ) -> io::Result<u64> {
        match (f_offset_start, f_offset_end) {
            (Some(start), Some(end)) => {
                if start > end || end > f_len {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid offset range",
                    ));
                }
                Ok(end - start)
            }
            (Some(start), None) => {
                if start > f_len {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid offset start",
                    ));
                }
                Ok(f_len - start)
            }
            (None, Some(end)) => {
                if end > f_len {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid offset end",
                    ));
                }
                Ok(end)
            }
            (None, None) => Ok(f_len),
        }
    }
}

/// Zero-copy IO with `splice(2)`.
///
/// Notice: see the [module-level documentation](crate) for known limitations.
pub struct SpliceIoCtx<R, W> {
    /// The `off_in` when splicing from `R` to the pipe, or the `off_out` when
    /// splicing from the pipe to `W`.
    offset: Offset,
    /// Target length to read from `R` then write to `W`.
    ///
    /// Default is `isize::MAX`, which means read as much as possible.
    size_to_splice: usize,

    /// Pipe used to splice data.
    pipe: Pipe,
    /// Bytes that have been read from `R` into pipe write side.
    has_read: usize,
    /// Bytes that have been written to `W` from pipe read side.
    has_written: usize,

    /// Whether need to flush `W` after writing.
    need_flush: bool,

    r: PhantomData<R>,
    w: PhantomData<W>,
}

impl<R, W> fmt::Debug for SpliceIoCtx<R, W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpliceIoCtx")
            .field("offset", &self.offset)
            .field("size_to_splice", &self.size_to_splice)
            .field("pipe", &self.pipe)
            .field("has_read", &self.has_read)
            .field("has_written", &self.has_written)
            .field("need_flush", &self.need_flush)
            .finish()
    }
}

impl<R, W> SpliceIoCtx<R, W> {
    #[inline]
    fn _prepare() -> io::Result<Self> {
        Ok(Self {
            offset: Offset::None,
            size_to_splice: isize::MAX as usize,
            pipe: Pipe::new()?,
            has_read: 0,
            has_written: 0,
            need_flush: false,
            r: PhantomData,
            w: PhantomData,
        })
    }

    #[inline]
    /// Prepare a new `SpliceIoCtx` instance.
    ///
    /// Can be used only when `R` and `W` are not files.
    ///
    /// ## Errors
    ///
    /// * Create pipe failed.
    pub fn prepare() -> io::Result<Self>
    where
        R: IsNotFile,
        W: IsNotFile,
    {
        Self::_prepare()
    }

    #[inline]
    /// Prepare a new `SpliceIoCtx` instance.
    ///
    /// Can be used only when `R` is a file.
    ///
    /// ## Arguments
    ///
    /// * `f_len` - File length.
    /// * `f_offset_start` - File offset start. Set to `None` to read from the
    ///   beginning.
    /// * `f_offset_end` - File offset end. Set to `None` to read to the end.
    ///
    /// ## Errors
    ///
    /// * Invalid offset.
    /// * Create pipe failed.
    pub fn prepare_reading_file(
        f_len: u64,
        f_offset_start: Option<u64>,
        f_offset_end: Option<u64>,
    ) -> io::Result<Self>
    where
        R: IsFile,
        W: IsNotFile,
    {
        Ok(SpliceIoCtx {
            offset: Offset::In(Some(f_offset_start.unwrap_or(0))),
            size_to_splice: Offset::calc_size_to_splice(f_len, f_offset_start, f_offset_end)?
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file size too large"))?,
            ..Self::_prepare()?
        })
    }

    #[inline]
    /// Prepare a new `SpliceIoCtx` instance.
    ///
    /// Can be used only when `W` is a file.
    ///
    /// ## Arguments
    ///
    /// * `f_len` - File length.
    /// * `f_offset_start` - File offset start. Set to `None` to write from the
    ///   beginning.
    /// * `f_offset_end` - File offset end. Set to `None` to write to the end.
    ///
    /// ## Errors
    ///
    /// * Invalid offset.
    /// * Create pipe failed.
    pub fn prepare_writing_file(
        f_len: u64,
        f_offset_start: Option<u64>,
        f_offset_end: Option<u64>,
    ) -> io::Result<Self>
    where
        R: IsNotFile,
        W: IsFile,
    {
        Ok(SpliceIoCtx {
            offset: Offset::Out(Some(f_offset_start.unwrap_or(0))),
            size_to_splice: Offset::calc_size_to_splice(f_len, f_offset_start, f_offset_end)?
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file size too large"))?,
            ..Self::_prepare()?
        })
    }

    /// Set the pipe size.
    ///
    /// See [`Pipe`]'s top level docs for more details.
    pub fn set_pipe_size(mut self, pipe_size: usize) -> io::Result<Self> {
        self.pipe.set_pipe_size(pipe_size)?;
        Ok(self)
    }
}

impl<R, W> SpliceIoCtx<R, W> {
    #[inline]
    /// Returns bytes that have been read from `R`.
    pub(crate) const fn has_read(&self) -> usize {
        self.has_read
    }

    #[inline]
    /// Returns bytes that have been written to `W`.
    pub(crate) const fn has_written(&self) -> usize {
        self.has_written
    }
}

impl<R, W> SpliceIoCtx<R, W>
where
    R: AsFd,
    W: AsFd,
{
    #[cfg_attr(
        any(
            feature = "feat-tracing-trace",
            all(debug_assertions, feature = "feat-tracing")
        ),
        tracing::instrument(level = "TRACE", skip_all, ret)
    )]
    /// Splicing data from `R` to pipe using `splice(2)`
    ///
    /// Call this after read done is accepted.
    ///
    /// ## Returns
    ///
    /// Whether still need to read more data from `R` to pipe.
    ///
    /// ## Errors
    ///
    /// - Error that `splice(2)` syscall returns.
    fn splice_drain(&mut self, r: &R, flags: SpliceFlags) -> io::Result<()> {
        crate::trace!(ctx = ?self, "start `splice_drain`");

        let Some(size_rest_to_splice) = self
            .size_to_splice
            .checked_sub(self.has_read)
            .and_then(NonZeroUsize::new)
        else {
            crate::info!(ctx = ?self, "data read reached target length");

            self.pipe.set_splice_drain_finished();

            return Ok(());
        };

        let Some(pipe_write_side_fd) = self.pipe.write_side_fd() else {
            crate::trace!(ctx = ?self, "pipe write side fd is closed");

            return Ok(());
        };

        crate::trace!(ctx = ?self, "need `splice_from_reader_to_pipe`");

        match splice(
            r.as_fd(),
            self.offset.off_in(),
            pipe_write_side_fd,
            None,
            size_rest_to_splice.get(),
            flags,
        )
        .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
        {
            Ok(0) => {
                crate::trace!(ctx = ?self, "no more data to read, read 0");

                // No more data to read, read 0.
                self.pipe.set_splice_drain_finished();

                return Ok(());
            }
            Ok(has_read) => {
                crate::trace!(ctx = ?self, "read {has_read} bytes from reader");

                self.has_read += has_read;
                self.size_to_splice -= has_read;

                Ok(())
            }
            Err(e) => {
                if e.kind() != io::ErrorKind::WouldBlock {
                    self.pipe.set_splice_drain_finished();
                }

                Err(e)
            }
        }
    }

    #[cfg_attr(
        any(
            feature = "feat-tracing-trace",
            all(debug_assertions, feature = "feat-tracing")
        ),
        tracing::instrument(level = "TRACE", skip(self, w), ret)
    )]
    /// Splicing data from pipe to `W` using `splice(2)`
    ///
    /// ## Returns
    ///
    /// If there is still data remained in pipe and need to call this again.
    ///
    /// ## Errors
    ///
    /// - Error that `splice(2)` syscall returns.
    fn splice_pump(&mut self, w: &W, flag: SpliceFlags) -> io::Result<bool> {
        const WRITE_DONE: bool = false;

        crate::trace!(ctx = ?self, "start `splice_pump`");

        let Some(pipe_read_side_fd) = self.pipe.read_side_fd() else {
            crate::debug!(ctx = ?self, "pipe read side fd is closed");
            // call splice after write done?
            return Ok(WRITE_DONE);
        };

        let Some(size_need_to_be_written) = self
            .has_read
            .checked_sub(self.has_written)
            .and_then(NonZeroUsize::new)
        else {
            if self.pipe.write_side_fd().is_none() {
                crate::trace!(ctx = ?self, "no more data to write from pipe to writer");

                self.pipe.set_splice_pump_finished();
            }

            // No data to write.
            return Ok(WRITE_DONE);
        };

        crate::trace!(
            "size_need_to_be_written: {}, has_written: {}, has_read: {}",
            size_need_to_be_written,
            self.has_written,
            self.has_read
        );

        crate::trace!(ctx = ?self, "need `splice_pump`");

        match splice(
            pipe_read_side_fd,
            None,
            w.as_fd(),
            self.offset.off_out(),
            size_need_to_be_written.get(),
            flag,
        )
        .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
        {
            Ok(0) => Err(io::ErrorKind::WriteZero.into()),
            Ok(has_written) => {
                self.need_flush = true;
                self.has_written += has_written;

                crate::trace!(ctx = ?self, "wrote {has_written} bytes to writer");

                Ok(
                    NonZeroUsize::new(self.has_read.checked_sub(self.has_written).ok_or_else(
                        || {
                            // If `has_written` is larger than `has_read`, may never stop.
                            // In particular, user's wrong implementation returning
                            // incorrect written length may lead to thread blocking.

                            io::Error::new(
                                io::ErrorKind::Other,
                                "`has_written` larger than `has_read`",
                            )
                        },
                    )?)
                    .is_some(),
                )
            }
            Err(e) => {
                if e.kind() != io::ErrorKind::WouldBlock {
                    self.pipe.set_splice_pump_finished();
                }

                Err(e)
            }
        }
    }

    #[inline]
    fn full_done(&self) -> bool {
        (self.pipe.splice_drain_finished()) && self.pipe.splice_pump_finished() && !self.need_flush
    }
}

impl<R, W> SpliceIoCtx<R, W>
where
    R: ReadFd,
    W: WriteFd,
{
    #[inline]
    #[cfg_attr(
        any(
            feature = "feat-tracing-trace",
            all(debug_assertions, feature = "feat-tracing")
        ),
        tracing::instrument(level = "TRACE", skip(self, r, w), ret)
    )]
    /// Copy data from `r` to `w` using `splice(2)`, but blocking mode.
    ///
    /// If you need bidirectional copy, you SHOULD spawn two
    /// [`thread`](std::thread)s, one for reading side `blocking_copy` and
    /// one for writing side `blocking_copy`.
    ///
    /// For blocking mode copy, the `r` and `w` MUST NOT be non-blocking mode,
    /// or dead loop waiting for I/O ready will lead to high CPU consumption.
    pub fn blocking_copy(mut self, r: &mut R, w: &mut W) -> io::Result<TrafficResult> {
        let ret = self._blocking_copy(r, w);

        TrafficResult::new_from_io_result(&self, None, ret)
    }

    fn _blocking_copy(&mut self, r: &mut R, w: &mut W) -> io::Result<()> {
        while !self.full_done() {
            if self.need_flush {
                crate::trace!(ctx = ?self, "start `flush`");

                w.flush()?;

                self.need_flush = false;
            }

            'splice_drain: loop {
                match self.splice_drain(r, SpliceFlags::empty()) {
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        // `r` is not ready for reading from?
                    }
                    e @ Err(_) => {
                        return e;
                    }
                    Ok(()) => {
                        // Successfully read from `r` to pipe.
                        break 'splice_drain;
                    }
                }
            }

            'splice_pump: loop {
                match self.splice_pump(w, SpliceFlags::empty()) {
                    Ok(true) => {
                        // Not finished, keep polling.
                    }
                    Ok(false) => {
                        // Pipe cleared.
                        break 'splice_pump;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        // `w` return EAGAIN?
                    }
                    Err(e) => {
                        return Err(e);
                    }
                }
            }
        }

        Ok(())
    }
}

impl<R, W> SpliceIoCtx<R, W>
where
    R: AsyncReadFd,
    W: AsyncWriteFd,
{
    #[inline]
    /// Copy data from `r` to `w` using `splice(2)`.
    pub async fn copy(
        mut self,
        mut r: Pin<&mut R>,
        mut w: Pin<&mut W>,
    ) -> io::Result<TrafficResult> {
        let ret = poll_fn(|cx| self.poll_copy(cx, r.as_mut(), w.as_mut())).await;

        TrafficResult::new_from_io_result(&self, None, ret)
    }

    #[cfg_attr(
        any(
            feature = "feat-tracing-trace",
            all(debug_assertions, feature = "feat-tracing")
        ),
        tracing::instrument(level = "TRACE", skip(self, cx, r, w), fields(pipe = ?self.pipe), ret)
    )]
    /// Polling version of [`copy`](Self::copy).
    ///
    /// Notice
    ///
    /// - The caller MUST NOT reuse the `SpliceIoCtx` after copy finished.
    /// - The traffic result is not returned, please use
    ///   [`TrafficResult::new_from_io_result`] after copy done to extract from
    ///   the ctx.
    pub fn poll_copy(
        &mut self,
        cx: &mut Context<'_>,
        mut r: Pin<&mut R>,
        mut w: Pin<&mut W>,
    ) -> Poll<io::Result<()>> {
        while !self.full_done() {
            crate::trace!(ctx = ?self, "loop `poll_copy`");

            ready!(self.poll_flush(cx, w.as_mut()))?;
            ready!(self.poll_splice_drain(cx, r.as_mut()))?;
            ready!(self.poll_splice_pump(cx, w.as_mut()))?;
        }

        Poll::Ready(Ok(()))
    }

    fn poll_flush(&mut self, cx: &mut Context<'_>, w: Pin<&mut W>) -> Poll<io::Result<()>> {
        if self.need_flush {
            crate::trace!(ctx = ?self, "start `poll_flush`");

            // Try flushing when the reader has no progress to avoid deadlock
            // when the reader depends on buffered writer.
            ready!(w.poll_flush(cx))?;

            self.need_flush = false;
        }

        Poll::Ready(Ok(()))
    }

    #[cfg_attr(
        any(
            feature = "feat-tracing-trace",
            all(debug_assertions, feature = "feat-tracing")
        ),
        tracing::instrument(level = "TRACE", skip(self, cx, r), ret)
    )]
    /// `poll_splice_drain` moves data from a socket (or file) to a pipe.
    ///
    /// Invariant: when entering `poll_splice_drain`, the pipe is empty. It is
    /// either in its initial state, or `poll_splice_pump` has emptied it
    /// previously.
    ///
    /// Given this, `poll_splice_drain` can reasonably assume that the pipe is
    /// ready for writing, so if splice returns EAGAIN, it must be because
    /// the socket is not ready for reading.
    fn poll_splice_drain(&mut self, cx: &mut Context<'_>, r: Pin<&mut R>) -> Poll<io::Result<()>> {
        crate::trace!(ctx = ?self, "start `poll_splice_drain`");

        if self.pipe.splice_drain_finished()
            // has data in pipe, skip reading from `r` into pipe
            || self.has_read != self.has_written
        {
            return Poll::Ready(Ok(()));
        }

        loop {
            // In theory calling splice(2) with SPLICE_F_NONBLOCK could end up an infinite
            // loop here, because it could return EAGAIN ceaselessly when the write
            // end of the pipe is full, but this shouldn't be a concern here, since
            // the pipe buffer must be sufficient (all buffered bytes will be written to
            // writer after this).

            ready!(r.poll_read_ready(cx))?;

            match r.try_io_read(|| self.splice_drain(&r, SpliceFlags::NONBLOCK)) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // The `r` is not ready for reading from, busy loop, though
                    // will return pending in next loop
                }
                r => break Poll::Ready(r),
            }
        }
    }

    #[cfg_attr(
        any(
            feature = "feat-tracing-trace",
            all(debug_assertions, feature = "feat-tracing")
        ),
        tracing::instrument(level = "TRACE", skip(self, cx, w), ret)
    )]
    fn poll_splice_pump(&mut self, cx: &mut Context<'_>, w: Pin<&mut W>) -> Poll<io::Result<()>> {
        crate::trace!(ctx = ?self, "start `poll_splice_pump`");

        loop {
            ready!(w.poll_write_ready(cx))?;

            match w.try_io_write(|| self.splice_pump(&w, SpliceFlags::NONBLOCK)) {
                Ok(true) => {
                    // Not finished, keep polling.
                }
                Ok(false) => {
                    // Pipe cleared.
                    break Poll::Ready(Ok(()));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // The `w` is not ready for being written to, busy loop,
                    // though will return pending in next loop
                }
                Err(e) => break Poll::Ready(Err(e)),
            }
        }
    }
}

impl<A, B> SpliceIoCtx<A, B>
where
    A: AsyncReadFd + AsyncWriteFd + IsNotFile,
    B: AsyncReadFd + AsyncWriteFd + IsNotFile,
{
    /// Copy data bidirectionally between `A` and `B`.
    ///
    /// Notice: the caller MUST NOT reuse the `SpliceIoCtx` after copy done.
    pub async fn copy_bidirectional(
        mut ctx_a_to_b: SpliceIoCtx<A, B>,
        mut ctx_b_to_a: SpliceIoCtx<B, A>,
        mut a: Pin<&mut A>,
        mut b: Pin<&mut B>,
    ) -> io::Result<TrafficResult> {
        let ret = poll_fn(|cx| {
            Self::poll_copy_bidirectional(
                cx,
                &mut ctx_a_to_b,
                &mut ctx_b_to_a,
                a.as_mut(),
                b.as_mut(),
            )
        })
        .await;

        TrafficResult::new_from_io_result(&ctx_a_to_b, Some(&ctx_b_to_a), ret)
    }

    /// Polling version of [`copy_bidirectional`](Self::copy_bidirectional).
    ///
    /// Notice
    ///
    /// - The caller MUST NOT reuse the `SpliceIoCtx` after copy finished.
    /// - The traffic result is not returned, please use
    ///   [`TrafficResult::new_from_io_result`] after copy done (successfully or
    ///   not).
    pub fn poll_copy_bidirectional(
        cx: &mut Context<'_>,
        ctx_a_to_b: &mut SpliceIoCtx<A, B>,
        ctx_b_to_a: &mut SpliceIoCtx<B, A>,
        mut a: Pin<&mut A>,
        mut b: Pin<&mut B>,
    ) -> Poll<io::Result<()>> {
        match (
            ctx_a_to_b.poll_copy(cx, a.as_mut(), b.as_mut()),
            ctx_b_to_a.poll_copy(cx, b.as_mut(), a.as_mut()),
        ) {
            (r @ Poll::Ready(Ok(_)), Poll::Ready(Ok(_))) => r,
            (e @ Poll::Ready(Err(_)), _) | (_, e @ Poll::Ready(Err(_))) => e,
            _ => {
                crate::trace!("`poll_copy_bidirectional` would block");
                Poll::Pending
            }
        }
    }
}

// === traits ===

/// Marker trait: indicate a file.
///
/// Since the compiler complains that conflicting implementations when we try to
/// implement `IsFile` for `T: ops::Deref<U>` when U: `IsFile`, you have to
/// implement this marker trait for your wrapper type over a file.
pub trait IsFile {}

impl<T> IsFile for &T where T: IsFile {}
impl<T> IsFile for &mut T where T: IsFile {}
impl<T> IsFile for Pin<&mut T> where T: IsFile {}

/// Marker trait: indicate not a file.
///
/// We have to introduce this because Rust does not allow the syntax `!IsFile`
/// (at least only limited to some builtin marker traits like `Send`),
pub trait IsNotFile {}

impl<T> IsNotFile for &T where T: IsNotFile {}
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
