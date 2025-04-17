//! `splice(2)` IO implementation.
//!
//! See [`splice`](rustix::pipe::splice) for more details.
//!
//! # References
//!
//!  - [Linux]
//!
//! [Linux]: https://man7.org/linux/man-pages/man2/splice.2.html

use std::{
    fmt,
    future::poll_fn,
    io,
    marker::PhantomData,
    os::fd::AsFd,
    pin::{pin, Pin},
    task::{ready, Context, Poll},
};

use rustix::pipe::{splice, SpliceFlags};
use tokio::{
    fs::File,
    io::{AsyncRead, AsyncWrite, Interest},
    net::{TcpStream, UnixStream},
};

use crate::pipe::Pipe;

pin_project_lite::pin_project! {
    /// Zero-copy IO with `splice(2)`.
    pub struct SpliceIoCtx<R, W> {
        need_flush: bool,
        read_done: bool,

        // offset of fd_in (should be a file)
        off_in: Option<u64>,

        // offset of fd_out (should be a file)
        off_out: Option<u64>,

        // target len
        target_len: Option<u64>,

        last_read: usize,
        has_read: usize,

        last_write: usize,
        has_written: usize,

        r: PhantomData<R>,
        w: PhantomData<W>,

        pipe: Pipe,
    }
}

impl<R, W> fmt::Debug for SpliceIoCtx<R, W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpliceIoCtx")
            .field("need_flush", &self.need_flush)
            .field("read_done", &self.read_done)
            .field("off_in", &self.off_in)
            .field("off_out", &self.off_out)
            .field("target_len", &self.target_len)
            .field("last_read", &self.last_read)
            .field("has_read", &self.has_read)
            .field("last_write", &self.last_write)
            .field("has_written", &self.has_written)
            .finish()
    }
}

impl<R, W> SpliceIoCtx<R, W> {
    #[inline]
    /// Prepare a new `SpliceIoCtx` instance.
    ///
    /// ## Arguments
    ///
    /// * `off_in` - Read offset against `R` (only when `R` is a file that makes
    ///   sense).
    /// * `off_out` - Write offset against `W` (only when `W` is a file that
    ///   makes sense).
    /// * `target_len` - Len of bytes to transfer from `R` to `W`.
    ///
    /// ## Errors
    ///
    /// * Fail to create a pipe. See [`Pipe::new()`].
    pub fn prepare(
        off_in: Option<u64>,
        off_out: Option<u64>,
        target_len: Option<u64>,
    ) -> io::Result<Self> {
        let pipe = Pipe::new()?;

        Ok(Self {
            need_flush: false,
            read_done: false,
            off_in,
            off_out,
            target_len,
            last_read: 0,
            has_read: 0,
            last_write: 0,
            has_written: 0,
            r: PhantomData,
            w: PhantomData,
            pipe,
        })
    }
}

impl<W> SpliceIoCtx<File, W> {
    #[inline]
    /// Create a new `SpliceIoCtx` instance.
    ///
    /// ## Arguments
    ///
    /// * `r` - [`tokio::fs::File`].
    /// * `f_offset_start` - File offset start. Set to `None` to read from the
    ///   beginning.
    /// * `f_offset_end` - File offset end. Set to `None` to read to the end.
    ///
    /// ## Errors
    ///
    /// * Fail to create a pipe.
    /// * Fail to get file length.
    /// * Invalid offset.
    pub async fn prepare_from_file(
        r: &File,
        f_offset_start: Option<u64>,
        f_offset_end: Option<u64>,
    ) -> io::Result<Self> {
        let file_len = r.metadata().await?.len();

        let target_len = match (f_offset_start, f_offset_end) {
            (Some(f_offset_start), Some(f_offset_end)) if f_offset_start > f_offset_end => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "error: invalid offset: `offset_from` is larger than `offset_to`",
                ))
            }
            (Some(f_offset_start), _) if file_len < f_offset_start => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "error: invalid offset: `offset_from` is out of bound of file length",
                ))
            }
            (_, Some(f_offset_end)) if file_len < f_offset_end => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "error: invalid offset: `offset_out` is out of bound of file length",
                ))
            }
            _ => f_offset_end.unwrap_or(file_len) - f_offset_start.unwrap_or(0),
        };

        Self::prepare(f_offset_start, None, Some(target_len))
    }
}

impl<R, W> SpliceIoCtx<R, W>
where
    R: AsyncReadFd,
    W: AsyncWriteFd,
{
    /// Copy data from `r` to `w` using `splice(2)`.
    pub async fn copy(mut self, mut r: Pin<&mut R>, mut w: Pin<&mut W>) -> io::Result<usize>
    where
        R: AsyncReadFd + Unpin,
        W: AsyncWriteFd + Unpin,
    {
        let mut this = pin!(self);

        poll_fn(|cx| Poll::Ready(ready!(this.as_mut().poll_copy(cx, r.as_mut(), w.as_mut())))).await
    }

    fn poll_fill_buf(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        r: Pin<&mut R>,
    ) -> Poll<io::Result<usize>> {
        let this = self.project();

        loop {
            ready!(r.poll_read_ready(cx))?;

            let has_read_result = r.try_io_read(|| {
                // ! overflow when target_len > u32::MAX on 32 bit system?
                splice(
                    r.as_fd(),
                    this.off_in.as_mut(),
                    this.pipe.write_fd(),
                    None,
                    this.target_len.unwrap_or(isize::MAX as u64) as usize - *this.has_read,
                    SpliceFlags::NONBLOCK,
                )
                .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
            });

            if has_read_result
                .as_ref()
                .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock)
            {
                continue;
            }

            if let Ok(&has_read) = has_read_result.as_ref() {
                if *this.last_read == has_read && has_read == 0 {
                    // no more data to read, read 0
                    *this.read_done = true;
                }

                *this.last_read = has_read;
                *this.has_read += has_read;

                // dbg!(format!(
                //     "current -> 0x{:x}",
                //     self.has_read
                // ));

                if *this.has_read >= this.target_len.unwrap_or(isize::MAX as u64) as usize {
                    // reached target length
                    *this.read_done = true;
                }
            }

            break Poll::Ready(has_read_result);
        }
    }

    fn poll_write_buf(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        w: Pin<&mut W>,
    ) -> Poll<io::Result<usize>> {
        let this = self.project();

        loop {
            ready!(w.poll_write_ready(cx)?);

            let has_written_result = w.try_io_write(|| {
                splice(
                    this.pipe.read_fd(),
                    None,
                    w.as_fd(),
                    this.off_out.as_mut(),
                    *this.has_read - *this.has_written,
                    SpliceFlags::NONBLOCK,
                )
                .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
            });

            if has_written_result
                .as_ref()
                .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock)
            {
                continue;
            }

            break Poll::Ready(has_written_result);
        }
    }

    /// Do zero-copy IO from `r` to `w` with `splice(2)`.
    pub fn poll_copy(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut r: Pin<&mut R>,
        mut w: Pin<&mut W>,
    ) -> Poll<io::Result<usize>> {
        loop {
            if !self.read_done && self.has_written == self.has_read {
                match self.as_mut().poll_fill_buf(cx, r.as_mut())? {
                    Poll::Ready(_) => {}
                    Poll::Pending => {
                        // Try flushing when the reader has no progress to avoid deadlock
                        // when the reader depends on buffered writer.
                        if self.need_flush {
                            ready!(w.poll_flush(cx))?;

                            self.need_flush = false;
                        }

                        return Poll::Pending;
                    }
                };
            }

            // need write data from pipe to writer
            while self.has_written < self.has_read {
                let has_written = ready!(self.as_mut().poll_write_buf(cx, w.as_mut()))?;

                if has_written == 0 {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "write zero byte into writer",
                    )));
                } else {
                    self.last_write = has_written;
                    self.has_written += has_written;
                    self.need_flush = true;
                }
            }

            // If pos larger than cap, this loop will never stop.
            // In particular, user's wrong poll_write implementation returning
            // incorrect written length may lead to thread blocking.
            debug_assert!(
                self.has_written <= self.has_read,
                "writer returned length larger than input slice"
            );

            if self.has_read == self.has_written && self.read_done {
                ready!(w.as_mut().poll_flush(cx))?;
                return Poll::Ready(Ok(self.has_written));
            }
        }
    }
}

impl<R, W> SpliceIoCtx<R, W>
where
    R: AsyncStreamFd,
    W: AsyncStreamFd,
{
    /// Copy data bidirectionally between `a` and `b` with `splice(2)`.
    ///
    /// This function returns a future that will read from both streams,
    /// writing any data read to the opposing stream. This happens in both
    /// directions concurrently.
    pub async fn copy_bidirectional(
        self,
        mut a: Pin<&mut R>,
        mut b: Pin<&mut W>,
    ) -> io::Result<(usize, usize)> {
        let mut io_a_to_b = pin!(self);
        let mut io_b_to_a = pin!(io_a_to_b.prepare_opposite_direction_ctx()?);

        poll_fn(|cx| {
            // Do not `ready!(io_a_to_b.poll_copy(cx, a.as_mut(), b.as_mut())?)`, or
            // `b_to_a` will never be polled.
            let a_to_b = io_a_to_b.as_mut().poll_copy(cx, a.as_mut(), b.as_mut())?;
            let b_to_a = io_b_to_a.as_mut().poll_copy(cx, b.as_mut(), a.as_mut())?;

            let a_to_b = ready!(a_to_b);
            let b_to_a = ready!(b_to_a);

            Poll::Ready(Ok((a_to_b, b_to_a)))
        })
        .await
    }

    #[inline]
    fn prepare_opposite_direction_ctx(&mut self) -> io::Result<SpliceIoCtx<W, R>> {
        debug_assert!(self.has_read == 0 && self.has_written == 0);

        // For bidirectional splice, must not be a file and offset is meaningless.
        // So we set them to None.
        self.off_in = None;
        self.off_out = None;

        Ok(SpliceIoCtx {
            need_flush: false,
            read_done: false,
            off_in: None,
            off_out: None,
            target_len: self.target_len,
            last_read: 0,
            has_read: 0,
            last_write: 0,
            has_written: 0,
            r: PhantomData,
            w: PhantomData,
            pipe: Pipe::new()?,
        })
    }
}

/// Marker trait: indicates a async-readable file descriptor.
pub trait AsyncReadFd: AsyncRead + AsFd {
    #[doc(hidden)]
    fn poll_read_ready(&self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    #[doc(hidden)]
    fn try_io_read<R>(&self, f: impl FnOnce() -> io::Result<R>) -> io::Result<R> {
        f()
    }
}

impl<T: AsyncReadFd + Unpin> AsyncReadFd for &mut T {
    fn poll_read_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        (**self).poll_read_ready(cx)
    }

    fn try_io_read<R>(&self, f: impl FnOnce() -> io::Result<R>) -> io::Result<R> {
        (**self).try_io_read(f)
    }
}

/// Marker trait: indicate a async-writable file descriptor.
pub trait AsyncWriteFd: AsyncWrite + AsFd {
    #[doc(hidden)]
    fn poll_write_ready(&self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    #[doc(hidden)]
    fn try_io_write<R>(&self, f: impl FnOnce() -> io::Result<R>) -> io::Result<R> {
        f()
    }
}

impl<T: AsyncWriteFd + Unpin> AsyncWriteFd for &mut T {
    fn poll_write_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        (**self).poll_write_ready(cx)
    }

    fn try_io_write<R>(&self, f: impl FnOnce() -> io::Result<R>) -> io::Result<R> {
        (**self).try_io_write(f)
    }
}

/// Marker trait: indicate a duplex stream like [`TcpStream`] or [`UnixStream`].
pub trait AsyncStreamFd: AsyncReadFd + AsyncWriteFd {}

macro_rules! impl_async_fd {
    (STREAM: $($ty:ty),+) => {
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

            impl AsyncStreamFd for $ty {}
        )+
    };
    (BLANKET: $($ty:ty),+) => {
        $(

            impl AsyncReadFd for $ty {}
            impl AsyncWriteFd for $ty {}

        )+
    };
}

impl_async_fd!(STREAM: TcpStream, UnixStream);
impl_async_fd!(BLANKET: File);
