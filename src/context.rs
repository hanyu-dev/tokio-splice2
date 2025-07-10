//! `splice(2)` IO context.

use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::{fmt, io};

use rustix::pipe::{splice, SpliceFlags};

use crate::io::{AsyncReadFd, AsyncWriteFd, IsFile, IsNotFile, SpliceIo};
use crate::pipe::Pipe;
use crate::traffic::TrafficResult;
use crate::utils::{Drained, Offset};

/// Splice IO context.
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
    /// Whether need to flush `W` after splicing.
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
    /// Set the target length of bytes to be copy from `R` to `W`.
    ///
    /// ## Notice
    ///
    /// You MAY want need
    /// [`prepare_reading_file`](Self::prepare_reading_file) or
    /// [`prepare_writing_file`](Self::prepare_writing_file) if `R` or `W` is a
    /// file.
    pub fn with_target_len(self, size_to_splice: usize) -> Self
    where
        R: IsNotFile,
        W: IsNotFile,
    {
        Self {
            size_to_splice,
            ..self
        }
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

    #[inline]
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
    pub const fn has_read(&self) -> usize {
        self.has_read
    }

    #[inline]
    /// Returns bytes that have been written to `W`.
    pub const fn has_written(&self) -> usize {
        self.has_written
    }

    #[inline]
    /// Returns the pipe size.
    pub const fn pipe_size(&self) -> NonZeroUsize {
        self.pipe.size()
    }

    // #[inline]
    // /// Returns if splice drain is finished.
    // pub(crate) const fn splice_drain_finished(&self) -> bool {
    //     self.pipe.splice_drain_finished()
    // }

    // #[inline]
    // /// Returns if splice pump is finished.
    // pub(crate) const fn splice_pump_finished(&self) -> bool {
    //     self.pipe.splice_pump_finished()
    // }

    #[inline]
    /// Returns if draining and pumping are both finished.
    pub(crate) const fn finished(&self) -> bool {
        self.pipe.splice_drain_finished() && self.pipe.splice_pump_finished()
    }

    #[inline]
    /// Returns the traffic result (client TX one).
    pub const fn traffic_client_tx(&self, error: Option<io::Error>) -> TrafficResult {
        TrafficResult {
            tx: self.has_written,
            rx: 0,
            error,
        }
    }

    #[inline]
    /// Returns the traffic result (client RX one).
    pub const fn traffic_client_rx(&self, error: Option<io::Error>) -> TrafficResult {
        TrafficResult {
            tx: 0,
            rx: self.has_read,
            error,
        }
    }

    #[inline]
    /// Builder pattern version of [`SpliceIo::new`].
    pub fn into_io(self) -> SpliceIo<R, W> {
        SpliceIo::new(self)
    }
}

impl<R, W> SpliceIoCtx<R, W>
where
    R: AsyncReadFd,
    W: AsyncWriteFd,
{
    #[cfg_attr(
        any(
            feature = "feat-tracing-trace",
            all(debug_assertions, feature = "feat-tracing")
        ),
        tracing::instrument(level = "TRACE", skip(cx, r), ret)
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
    ///
    /// Will close pipe write side when no more data to read or when I/O error
    /// occurs except EINTR / EAGAIN.
    pub(crate) fn poll_splice_drain(
        &mut self,
        cx: &mut Context<'_>,
        r: Pin<&mut R>,
        ideal_len: Option<NonZeroUsize>,
    ) -> Poll<io::Result<Drained>> {
        crate::trace!("`poll_splice_drain`");

        let Some(pipe_write_side_fd) = self.pipe.write_side_fd() else {
            // Has `set_splice_drain_finished`, no need to close pipe write side again.
            // self.pipe.set_splice_drain_finished();

            return Poll::Ready(Ok(Drained::Done));
        };

        let Some(size_rest_to_splice) = self
            .size_to_splice
            .checked_sub(self.has_read)
            .map(|l| match ideal_len {
                Some(len) => len.get().min(l),
                None => l,
            })
            .and_then(NonZeroUsize::new)
        else {
            self.pipe.set_splice_drain_finished();

            return Poll::Ready(Ok(Drained::Done));
        };

        loop {
            // In theory calling splice(2) with SPLICE_F_NONBLOCK could end up an infinite
            // loop here, because it could return EAGAIN ceaselessly when the write
            // end of the pipe is full, but this shouldn't be a concern here, since
            // the pipe buffer must be sufficient (all buffered bytes will be written to
            // writer after this).
            ready!(r.poll_read_ready(cx))?;

            match r.try_io_read(|| {
                splice(
                    r.as_fd(),
                    self.offset.off_in(),
                    pipe_write_side_fd,
                    None,
                    size_rest_to_splice.get(),
                    SpliceFlags::NONBLOCK,
                )
                .map(NonZeroUsize::new)
                .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
            }) {
                Ok(Some(drained)) => {
                    self.has_read += drained.get();
                    self.size_to_splice -= drained.get();

                    break Poll::Ready(Ok(Drained::Some(drained)));
                }
                Ok(None) => {
                    self.pipe.set_splice_drain_finished();

                    break Poll::Ready(Ok(Drained::Done));
                }
                Err(e) => {
                    match e.kind() {
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => {
                            // The `r` is not ready for reading from, busy loop,
                            // though will return pending in next loop
                            // continue;
                        }
                        _ => {
                            self.pipe.set_splice_drain_finished();

                            break Poll::Ready(Err(e));
                        }
                    }
                }
            }
        }
    }

    #[cfg_attr(
        any(
            feature = "feat-tracing-trace",
            all(debug_assertions, feature = "feat-tracing")
        ),
        tracing::instrument(level = "TRACE", skip(cx, w), ret)
    )]
    /// `poll_splice_pump` moves data from a pipe to a socket (or file).
    ///
    /// Will close pipe read side when no more data to write or when I/O error
    /// occurs except EINTR / EAGAIN.
    ///
    /// Will keep pumping data until the pipe is empty (returns
    /// `Poll::Ready(Ok(())`), the socket is not ready (returns
    /// `Poll::Pending`) or error occurs.
    pub(crate) fn poll_splice_pump(
        &mut self,
        cx: &mut Context<'_>,
        w: Pin<&mut W>,
    ) -> Poll<io::Result<()>> {
        crate::trace!("`poll_splice_pump`");

        let Some(pipe_read_side_fd) = self.pipe.read_side_fd() else {
            return Poll::Ready(Ok(()));
        };

        loop {
            let Some(size_need_to_be_written) = self.has_read.checked_sub(self.has_written) else {
                // If `has_written` is larger than `has_read`, may never stop.
                // In particular, user's wrong implementation returning
                // incorrect written length may lead to thread blocking.

                self.pipe.set_splice_pump_finished();

                break Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::Other,
                    "`has_written` larger than `has_read`",
                )));
            };

            let Some(size_need_to_be_written) = NonZeroUsize::new(size_need_to_be_written) else {
                if self.pipe.splice_drain_finished() {
                    self.pipe.set_splice_pump_finished();
                }

                break Poll::Ready(Ok(()));
            };

            ready!(w.poll_write_ready(cx))?;

            match w.try_io_write(|| {
                splice(
                    pipe_read_side_fd,
                    None,
                    w.as_fd(),
                    self.offset.off_out(),
                    size_need_to_be_written.get(),
                    SpliceFlags::NONBLOCK,
                )
                .map(NonZeroUsize::new)
                .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
            }) {
                Ok(Some(has_written)) => {
                    // Go to next loop to check if there is more data
                    self.has_written += has_written.get();
                    self.need_flush = true;
                }
                Ok(None) => {
                    break Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
                }
                Err(e) => {
                    match e.kind() {
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => {
                            // The `r` is not ready for reading from, busy loop,
                            // though will return pending in next loop
                            // continue;
                        }
                        _ => {
                            self.pipe.set_splice_drain_finished();

                            break Poll::Ready(Err(e));
                        }
                    }
                }
            }
        }
    }
}
